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

### Round 3: The client-side resolver

Every client renders templates through `libbeachcomber::resolve`, so its cost is every status line's. Measured two ways: a status line of 12 widgets in the shape scanline's one-tree config takes (a theme group of eleven leaves read by eleven widgets, derived leaves the widgets share, the width in `env`), each widget its own `resolve`, the template data local and five reads from a daemon, minimum over 100 renders; and `cargo bench --bench resolve`. The host was at load average 100–200 throughout, so compare the figures with each other, not with the tables below.

#### 3.1 Render each node once per resolution

**Problem:** The resolver memoised fetches per `(key, path)`, so a ref named twice was one round trip, but not renders. A widget that reads the theme and two derived leaves that read it too rendered the theme's eleven leaves four times in one resolution.

**Fix:** A second memo keeps what each node resolved to, a render failure included, for the rest of the top-level resolution. The frame stack is checked first, so a node asked for while it is being rendered is still a cycle. The dependency list and the oldest-input age belong to the resolution, so a read the memo answers leaves both as they were: its inputs were counted when it first rendered.

**Result:** the status line 8.0 → 7.0 ms a render with a minimal environment, and 22 → 17 ms with 140 variables, since fewer renders also meant fewer copies of the environment (3.2). `resolve/widget/1` 0.92 → 0.61 ms, `resolve/widget/140` 4.5 → 1.4 ms.

**Rule for future changes:** Both memos live for one top-level call and no longer. A memo across calls would hand one caller's `env` and directory to the next: each of scanline's widgets reads its own `env.SCANLINE_WIDTH`.

#### 3.2 Share the caller's environment across renders

**Problem:** So that `env[var]` and `env | items` work, every render put the caller's whole environment into its context: a fresh JSON object of every variable, converted again into an engine value. With 140 variables that was more than half of the status line's render time.

**Fix:** The environment is one engine value per resolution, built once in name order in `Resolver::new`; each render's context holds that value, and minijinja values are reference-counted, so binding it copies nothing.

**Result:** the status line with 140 variables 17 → 8.0 ms a render, within 1 ms of a minimal environment's 7.0 ms. `resolve/widget/140` 1.4 → 0.48 ms.

**What remains:** a resolution still copies the environment twice, once into `EngineCtx` (the environment of `exec()`, `read` and `lup`) and once into the template value. That is once per resolution, not per render: `resolve/env_read/140` is about 70 µs above `resolve/env_read/1`, before and after.

**Rule for future changes:** What every render of a resolution reads the same — the environment, the directory — is built once in `Resolver::new`, never in `bind` or per render.

### Round 4: Loading a configuration

Every daemon start, config reload and `comb check config` loads the configuration (`ProviderRegistry::load`), and so does every test that builds a registry (`ProviderRegistry::shipped`). Phase 7 moved the built-in providers into `defaults/*.toml` (3.7 KB → 17.3 KB of Sources and templates), and the load became the test suite's largest cost. A profile of a debug load found most of it in work done again: an engine environment built for every line read, the engine's global names collected for every line, the seed planned twice, the abstract provider's lines analysed once per provider table.

**Fix:** what a load reads lines in is built once per load and handed down; nothing is kept across loads.

| Change | `load/scanline` debug | release |
|---|---:|---:|
| Before (`feat/one-tree` at `72f1ff9c`, with the bench) | 21.8 ms | 11.3 ms |
| A line is read in a `Probe`: an engine with its global names, collected once | 19.8 | 10.6 |
| One engine per load for every config Source's lines (`config_source::load_probe`) | 17.4 | 9.7 |
| One engine per load that every `return` render copies (`decorators::return_env`) | 16.1 | 9.1 |
| The seed plan reads the loaded `Config` and the registry's compiled provider lines | 14.4 | 8.0 |
| Each file's labels (a `$HOME` look-up on disk each) worked out once per plan | 14.2 | 7.8 |
| Each file's `[client]` tree made an engine value once, not once per provider | 13.6 | 7.5 |
| Every `return` render shares the load's variables | 13.1 | 7.3 |
| A Source's `data` line read once for its refs and its calls (`eval::refs_in`, `calls_in`) | 12.8 | 7.1 |
| A provider table that sets no line shares the abstract provider's chains | 11.4 | 6.1 |

Two changes are outside the bench: a daemon start and a reload write the seed their load planned (it was planned a second time), and `comb check config` loads the configuration `comb` composed at start (it read and parsed every file again).

**Result:** measured 2026-10-26, the base and the new build run alternately (each round runs every build once, in turn), on the 12-core Mac. Medians, the minimum in brackets. The benches and the CLI at load 7–13; the suite at load 17–137, most of it the suite's own.

| What | Before | After | Change |
|---|---:|---:|---:|
| `load/shipped`, debug (`cargo bench --profile dev --bench load`) | 15.3 ms | 6.8 ms | −55% |
| `load/scanline`, debug | 21.8 ms | 11.4 ms | −48% |
| `load/shipped`, release | 8.0 ms | 3.9 ms (3.6) | −51% |
| `load/scanline`, release | 11.3 ms | 6.1 ms (6.0) | −46% |
| `comb daemon` start to ready (`--ready-fd`), release | 29.6 ms (28.3) | 17.3 ms (16.3) | −42% |
| `comb check config`, release | 20.8 ms (19.7) | 13.4 ms (12.8) | −36% |
| `cargo nextest run --workspace`, wall | 11.8 s (11.1) | 10.7 s (10.1) | −9% |
| `cargo nextest run --workspace`, CPU-seconds | 97.0 (96.0) | 81.8 (79.7) | −16% |

Three rounds for the benches, 15 for the CLI, five for the suite (2508 tests each time). The daemon and `check config` ran on an isolated `$HOME`, `$XDG_CONFIG_HOME` and socket, with an empty `config.toml` and scanline's drop-in in `conf.d`; each daemon was stopped by its pid once it signalled ready. `load/parse` (the shipped files parsed, 1.6 ms debug, 0.25 ms release) did not change.

**What remains** in a release `load/scanline`, by profile:

- **Linting each client template leaf, about a quarter.** Each leaf is linted once, which is the work; but its text is scanned by several passes (its refusals, its imports, its caching, its form), each scanning it again. One scan shared by the passes is the next step, and the larger one in a debug build, where the scanner is unoptimised.
- **Rendering each provider's `template` line over each file's `[client]` tree, about a quarter.** One render per (file, provider) is the design; about a third of it is compiling the same line again for each. Compiling it once needs the evaluator to hand back a compiled line, which a statements line (rewritten to owned text) cannot borrow from: a change to `eval::evaluate`, not to the load.
- **Linting each Source's lines (about 14%) and recording each `return` (about 6%)**: each line once.
- **Parsing the TOML (about 8%)**: the bench's own; a daemon parses its files once.
- **Labels (about 4%)**: two `$HOME` look-ups per file in the plan and one in the composition.

**Rule for future changes:** what a load reads every line in (an engine, its global names, the configuration, the provider lines, the seed) is built once per load and handed to whatever needs it. Nothing is cached across loads, and nothing that depends on the configuration is a process-wide static.

---

## Current Performance Profile

### Provider Execution Time Tiers

| Tier | Time | Providers | Method |
|---|---|---|---|
| **Nanosecond** (< 1µs) | 395ns - 749ns | user, load, hostname, uptime, gcloud_configs, aws_profiles | libc calls, file reads, env vars |
| **Microsecond** (1-100µs) | ~1-50µs | terraform, python, asdf, direnv (no direnv binary) | File existence checks + reads |
| **Millisecond** (1-10ms) | 2-6ms | network (2ms), git (5.6ms), battery (6ms) | 1 process spawn each |
| **Slow** (10-50ms) | 10-50ms | mise, direnv (with direnv), Sources that call `exec()` | Process spawn (user-defined) |

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

### Read validity: the phase 4 baseline

Measured 2026-10-26 at `d744809`, before any phase 4 change, on a 12-core Mac whose load average was between 55
and 150 throughout. Under that load a median is several times the uncontended cost, so the minimum is given beside
it; compare a later run with one taken at the same load, interleaved where possible.

`cargo bench --bench validity -- warm_get_percentiles`: a warm `get` over the raw socket, one connection, the
daemon's own startup path in-process (native watcher), 2000 interleaved rounds (load ~115):

| Read | min | p50 | p90 | p99 |
|---|---:|---:|---:|---:|
| config Source, `poll` only (0 watch paths) | 32 µs | 281 µs | 5.3 ms | 28.6 ms |
| config Source, `watch` of 1 path | 28 µs | 253 µs | 4.7 ms | 24.7 ms |
| config Source, `watch` of 8 paths | 29 µs | 238 µs | 4.6 ms | 22.7 ms |
| `git.branch` (git `head` ran on every read) | 66 µs | 753 µs | 13.6 ms | 48.8 ms |
| `git`, the whole namespace | 97 µs | 798 µs | 11.9 ms | 53.1 ms |

`cargo bench --bench validity -- second_get_after_cold` (audit P9: a `root = lup(…)` plus `watch(…)` Source, the
cold `get`, then the next ones, in fresh daemons):

- **P9 does not reproduce.** The second `get` took 0.06–0.69 ms in 12 of 12 fresh in-process release daemons (load
  ~75), 0.15–0.86 ms in 6 of 6 spawned debug `comb daemon`s, and 0.1–0.5 ms in 6 runs of the test harness's
  in-process polling daemon. A second release run at load ~150 gave 0.08–2.1 ms, 6 of 12 under 1 ms.
- **What the first demand does cost is the scheduler loop.** It registers the instance's watches inside the loop, so
  an `introspect watches` sent just after the cold `get` waited 5.5–56 ms (median 16 ms) for a native registration
  to finish. A request never waits on the loop, but everything the loop answers does: `introspect`, the `status`
  op's lifecycle and failure snapshots, file events, polls and reloads.

`cargo bench --bench prompt_race -- read_after_mutation` (a change on disk, then at once a `get`, watcher on, 100
trials each):

| Change, then read | Answered the new value | read p50 | read p99 |
|---|---:|---:|---:|
| `.git/HEAD` written, `git.branch` | 100/100 (git `head` ran on every read) | 0.12 ms | 0.37 ms |
| `state.json` rewritten, a config Source with `watch('state.json')` | 8/100 | 0.13 ms | 2.8 ms |

`convergence_L` (`.git/HEAD` written until the cache shows the new branch, through the watcher): 7.6 ms mean.

The CLI (`hyperfine -N --warmup 20 --runs 200`, release, an isolated daemon, load ~57): `comb get git.branch` has a
median of 14.8 ms (min 7.3 ms) against 14.2 ms (min 6.4 ms) for `comb get hostname.name`, the process floor.

### Read validity: a warm read checks its record's files

Since `a warm read checks its record's files` a read stats a Source record's inputs before it answers from it, and
git's `head` no longer runs on every read. Measured 2026-10-26 against the baseline binary above, interleaved run for
run, at load 140-200 (so read the minimum beside the median):

- **Warm `get`, raw socket** (`warm_get_percentiles`, two runs of each): `git.branch` p50 106-108 µs (baseline
  266 µs), min 41 µs (77-78 µs); `git` p50 184-185 µs (329-332 µs), min 84-86 µs (110-114 µs). The config Sources
  are unchanged within noise (p50 98-121 µs against 102-122 µs).
- **The stat pass**: `watch8` against `watch0` adds 8-10 µs at the minimum and 18-23 µs at the median, 1.1-2.8 µs
  per input. One `lstat(2)` per input that is not a symlink; `stat_pass/lstat` (a bare `lstat`) measured 9-12 µs
  on this host at this load, against the ~1 µs of an idle one.
- **Read after a change** (`read_after_mutation`, four runs): 100/100 in every run, for the `.git/HEAD` write and for
  the `state.json` rewrite (baseline 8/100), with the watcher on and with it off. The read that finds the change
  has a p50 of 0.21-0.97 ms for `git.branch` and 0.26-1.98 ms for the config Source (under 1 ms in 15 of the 16
  runs and modes); its p99 is the load's (1.7-56 ms).
- **CLI** (`hyperfine -N --warmup 20 --runs 200`, three runs of each): `comb get git.branch` minus `comb get
  hostname.name` at the median was +0.2, +1.0 and +5.5 ms (baseline +1.2 to +2.3 ms); at the minimum 6.4-7.2 ms
  against 6.6-6.9 ms.
- **Size**: `Option<Validity>` is 48 bytes per record, beside its input paths.

### Read validity: phase 4 measured

Measured 2026-10-27 at `1f53f763` (the P4.1 baseline is `5d92ed4`; the exec-cache baseline `14bee04`), release builds, on
the 12-core Mac once it had gone quiet: load average 5-26 (each row gives its own; the higher figures are the
benches' own work, the load before them was 6-16). Every comparison is **alternating** (A, B, A, B), baseline then
current, each with its own isolated daemon (`HOME`, `XDG_*` and socket in a scratch directory). The earlier tables
above were taken at load 55-200 and are superseded where this one gives a number.

| # | Target | Result | Met |
|---|---|---|---|
| T4.1 | warm raw-socket `get git.branch` p50 <= 0.25 ms; `get git` <= 0.4 ms; stat pass <= 2 us per input | `warm_get_percentiles`, 2000 rounds, load 26: `git.branch` p50 39 us (baseline 68 us), min 29 us (52); `git` p50 72 us (97), min 60 us (79); the config Sources unchanged (38 vs 37 us at 0 and 1 watch path); 8 watch paths add 9 us at p50 (45 vs 36 us), about 1.1 us per input. Stat pass (`stat_pass/`, criterion, load 17): 1.46 us for 1 input, 5.74 us for 4, 14.4 us for 10, so 1.4 us per input over a bare `lstat(2)` of 1.25 us | met |
| T4.2 | `comb get git.branch` within 1 ms of `comb get hostname.name` | `hyperfine -N --warmup 20 --runs 100`, load 5.6: current 4.4 vs 4.4 ms and 4.4 vs 4.3 ms (+0.0, +0.1); baseline 3.9 vs 3.8 and 3.8 vs 3.7. The process floor itself is 0.6 ms higher now (4.4 against 3.8 ms; user time 2.6 against 2.2 ms), which is the larger binary's start-up, not validity | met |
| T4.3 | after a change, 100/100 new values; the first read <= 1 ms | `read_after_mutation`, two runs, load 7-9: 100/100 in every mode for the `.git/HEAD` write and the `state.json` rewrite (baseline: 8/100 for the latter); read p50 0.07-0.12 ms for `git.branch`, 0.15-0.16 ms for the config Source; p99 <= 0.34 ms. Watcher on and off alike | met |
| T4.4 | default `get` within budget + 5 ms; `--wait 0` within 1 ms; `--wait forever` about the run | `budget_accuracy`, 300 ms Source, 50 ms budget, two runs of 4 rounds, load 13-16: default read 51.7-53.3 ms, `--wait forever` 315-336 ms (run 300 ms); **`--wait 0` 1.4-1.5 ms in 8 of 8** | default and `forever` met; **`--wait 0` missed by 0.4-0.5 ms** |
| T4.5 | the second `get` after a cold run <= 1 ms in 4 of 4 fresh daemons | `second_get_after_cold`, two runs of 12 fresh daemons each, load 16: 0.047-0.114 ms, 24 of 24 (baseline 0.038-0.070 ms, 24 of 24; P9 never reproduced at this load). An `introspect watches` sent just after the cold `get` now answers in 0.03-0.04 ms (baseline: 1.8-5.2 ms, waiting on the scheduler loop); two of the 24 took 0.8 and 1.2 ms, the first daemons of one run | met |
| T4.6 | cold `comb get git` <= 60 % of today's | `t46.py`, fresh daemons, load 6-13, medians: 37.4 vs 61.9 ms (60 %) and 32.6 vs 81.3 ms (40 %); with the logging `git` wrapper (11 `git` processes either way): 176 vs 240 ms and 105 vs 214 ms. The first pair is at the limit | met, at the limit in one of two runs |
| T4.7 | idle: 0 kernel watches after decay; a 2 s registration leaves no heartbeat gap > 10 ms | `a_watch_goes_when_its_instance_decays_and_a_read_brings_it_back` and `a_memo_decays_lets_go_of_its_watch_and_is_evicted` pass; `heartbeat_gap_during_a_slow_registration` (ignored by default, run at load 11): latest tick 2.5 ms late, a status request sent with the demand waited 1.3 ms | met |
| T4.8 | validity <= 64 B per record | the `size_of::<Option<Validity>>()` assertion in `src/cache.rs` passes; 48 B | met |
| T7.8 | the stat pass <= 2 us per input (gcloud's 3 inputs, ~5 us per file at load ~110) | 1.4 us per input at 1, 4 and 10 inputs (above); 3 inputs would be about 4.3 us in all. The 5 us per file was the load, as a bare `lstat` was then 9-12 us. Not measured on `gcloud` itself | met on the stat pass; not re-measured through gcloud |

**T4.4 `--wait 0` is still missed at a quiet machine, and it is not noise: 1.4-1.5 ms every time.** The cause is
visible by swapping the Source's command. With a Source whose data is a constant (`{{ 1 }}`) the stale `--wait 0` read
answers in 0.3 ms; with `exec('echo 1')` or `exec('sleep 0.3; echo 1')` it answers in 1.5 ms. So about 1.2 ms of the
read is the start of the run's process (the spawn on macOS), which the answer waits behind though it does not wait for
the run. The same read against a daemon with its scheduler running answers in 0.15 ms, most likely because the
scheduler has already refreshed the record (the answer was not stale); the figure above is the bench's schedulerless
server, where the read starts the run.

### The watch fan-in: one `poll(2)` (phase 3)

Measured 2026-10-26 at the commit that made a watch wait on one `poll(2)`, on the same 12-core Mac, twice: once at a
load average of about 28, and once between 250 and 320, where every tail is that load's and the minimum is the cost
itself. Before it, the fan-in read its streams in turn for up to 50 ms each, so a change on the last of `k` streams
took up to `(k − 1) × 50 ms` (950 ms at `k = 20`), a cancel up to 50 ms a stream, and an idle watch woke 20 times a
second a stream.

`cargo bench --bench watch` (release, a spawned `comb daemon`, the client `ResolvedWatch`; a watch of `k` deps reads
a template naming `k − 1` refs, and each change is a `put` to the last). At load ~28:

| Measurement | min | p50 | p90 | p99 | max | Target |
|---|---:|---:|---:|---:|---:|---|
| `put` to `next` returning it, k = 1 | 0.059 ms | 0.085 ms | 0.117 ms | 0.265 ms | 0.505 ms | p50 ≤ 1 ms, p99 ≤ 5 ms |
| the same, k = 5 | 0.163 ms | 0.235 ms | 0.500 ms | 1.106 ms | 1.523 ms | as above |
| the same, k = 20 | 0.450 ms | 0.617 ms | 0.778 ms | 1.463 ms | 1.683 ms | as above |
| a `put` alone, for scale | 0.012 ms | 0.027 ms | 0.059 ms | 0.115 ms | 0.151 ms | |
| opening a warm 20-dep watch | 0.624 ms | 0.790 ms | 0.969 ms | 1.900 ms | 2.939 ms | ≤ 5 ms |
| `cancel()` to a blocked `next(None)` returning | 0.010 ms | 0.019 ms | 0.028 ms | 0.080 ms | 0.417 ms | ≤ 2 ms |
| `next(Some(10 ms))`, time past the deadline | 0.015 ms | 1.014 ms | 1.030 ms | 1.298 ms | 1.424 ms | within 2 ms |
| `next(Some(50 ms))`, time past the deadline | 0.038 ms | 0.969 ms | 1.040 ms | 1.068 ms | 2.814 ms | within 2 ms |

At load 250–320 (two runs, given as ranges):

| Measurement | min | p50 | p90 | p99 |
|---|---:|---:|---:|---:|
| `put` to `next` returning it, k = 1 | 0.08 ms | 0.30–0.65 ms | 3.0–14 ms | 41–66 ms |
| the same, k = 5 | 0.24 ms | 0.85–1.17 ms | 3.3–22 ms | 53–101 ms |
| the same, k = 20 | 0.72 ms | 2.9–3.3 ms | 24–39 ms | 248–259 ms |
| a `put` alone, for scale | 0.02 ms | 0.05–0.13 ms | 0.4–2.6 ms | 9–23 ms |
| opening a warm 20-dep watch | 0.79 ms | 2.6–4.1 ms | 44–109 ms | 254–394 ms |
| `cancel()` to a blocked `next(None)` returning | 0.016 ms | 0.034 ms | 0.061 ms | 0.21 ms |
| `next(Some(10 ms))`, time past the deadline | 0.04 ms | 0.90–1.05 ms | 1.2–1.7 ms | 2.5–3.2 ms |
| `next(Some(50 ms))`, time past the deadline | 0.07 ms | 0.99–1.09 ms | 1.4–5.7 ms | 3.2–25 ms |

- **Delivery no longer depends on which stream changed;** what it costs is the re-resolve, one round trip per key the
  value reads, over the one connection the watch holds: about 25–35 µs a key, 0.45–0.72 ms at `k = 20`. At load
  250–320 a `put` alone has a 9–23 ms p99, so no p99 there can meet 5 ms.
- **Idle:** a 20-dep watch's `next(Some(10 s))` with nothing happening made 2 system calls (macOS keeps no count of
  voluntary context switches; a thread that wakes must make another call to sleep again) and used 0.3 ms of CPU.
  A `comb watch` of a 20-dep key left idle for 60 s after its first frame used 7.94 ms of CPU and 21 voluntary
  context switches over its whole life, against 7.82 ms and 21 for one stopped at its first frame: the minute cost
  0.12 ms and no wake.
- **A deadline** is never answered early; the p50 overshoot of about 1 ms is macOS's timer leeway on `poll`.

### Exec caching: the phase 5 baseline

Measured 2026-10-26 at `14bee04`, before any phase 5 change, on the same 12-core Mac, twice: at a load average of
16–21, and at 41–52. The lower is given first.

`cargo bench --bench resolve` (the resolver over a map `Fetch`, no daemon). `env_read` is `{{ env.X }}`, the floor a
memo hit's frame and hash are measured against (T5.1); `exec` is `{{ 'echo x' | exec() }}`, one `sh -c` spawn, what a
memo exists to skip. A first run at load 40–150 was 10–30 times slower in every case and is not given.

| Case | 1 variable | 140 variables |
|---|---:|---:|
| `resolve/env_read` | 82 µs | 135 µs |
| `resolve/exec` | 11.3 ms | 7.5 ms |
| `resolve/widget` | 0.64 ms | 0.85 ms |

The CLI: `hyperfine -N --warmup 20 --runs 100`, the release build, an isolated daemon (its `$HOME`,
`$XDG_CONFIG_HOME`, `$XDG_STATE_HOME` and socket in a scratch directory, so it writes and seeds the managed defaults),
and stand-ins for `kubectl` and `talosctl` first on `PATH`: the suite's `stub_bin` scripts, `#!/bin/sh` that appends
one line to `$COMB_STUB_LOG` when it is set and exits 1, so `kubecontext.context` is absent, as on a machine with no
context. Each command was run once before `hyperfine` started.

| Command | median | min | at load 41–52: median / min |
|---|---:|---:|---:|
| `comb get kubecontext.context` | 100.9 ms | 51.0 ms | 266.5 / 137.7 ms |
| `comb get hostname.name` (the process floor, T5.2) | 10.8 ms | 6.8 ms | 19.0 / 7.7 ms |
| `comb status` | 283.1 ms | 179.6 ms | 912.7 / 448.2 ms |

- **One stub spawn** is the difference between the first two rows: 90 ms at the median, 44 ms at the minimum. The stub
  is a script run under `sh -c`, so a spawn is two shells; a real `kubectl` costs tens of milliseconds more.
- **Processes per `comb status`** (`$COMB_STUB_LOG` over one run): 3. `kubectl config current-context` runs twice,
  once for the `context` row and once more inside `namespace`, whose template reads `kubecontext.context` (each row is
  a resolution of its own), and `talosctl config info -o json` once. T5.3 starts from 3, not the 2 the phase 5 plan
  counted. `comb get kubecontext.context` runs 1.

### Exec caching: phase 5 measured

Measured 2026-10-26 at `f5777a89` against the baseline binary `14bee04`, on the same Mac. The machine was far more
loaded than during the baseline: load average 105-126 on 12 cores for the CLI and daemon runs, so absolute numbers are
2-3 times the baseline's. Every comparison below is **interleaved** (each round runs every command once, base and new in
turn, each with its own isolated daemon and the counting stubs), so a load swing falls on both alike; read the
differences, not the absolute times. Release builds throughout.

The CLI, 120 interleaved rounds (`comb get` and `comb status`, stand-in `kubectl` and `talosctl` exiting 1):

| Command | base median / min | phase 5 median / min |
|---|---:|---:|
| `comb get kubecontext.context` | 80.4 / 47.6 ms | 28.2 / 16.9 ms |
| `comb get hostname.name` (the process floor) | 24.9 / 14.1 ms | 26.7 / 15.5 ms |
| `comb status` | 223.9 / 123.5 ms | 55.7 / 24.4 ms |

Processes per warm `comb status`: 3 at `14bee04`, **0** now. Per `comb get kubecontext.context`: 1, now 0.

A miss costs what a spawn costs, and the write-back adds nothing visible: 80 rewrites of the kubeconfig, each followed
by a `comb get kubecontext.context`, took a median 86.3 ms (min 54.2) against 86.5 ms (min 52.0) for the baseline's
uncached read, which spawns every time (load 108-126).

| # | Target | Result | Met |
|---|---|---|---|
| T5.1 | memo hit <= 20 us over `{{ env.X }}` | 62 vs 46 us (+16) at one variable; 152 vs 131 us (+21) at 140 variables; p50, alternating runs at load about 47 (`benches/resolve.rs`). A re-run at load 112 was too noisy to use (the hit read faster than the floor) | met at 1 variable; 1 us over at 140 |
| T5.2 | `get kubecontext.context` warm within 1 ms of `get hostname.name` | median +1.5 ms (28.2 vs 26.7), min +1.4 ms at load 105-126; an earlier 40-round run gave +4.8 ms median, +1.5 ms min; the hit also runs a `stat` pass and a filter pipeline the floor does not | missed by about 0.5-4 ms; noise-dominated, the minimum is steady at +1.4 ms |
| T5.3 | warm `comb status`: 0 processes | 0 (3 at baseline); `a_warm_status_spawns_no_process` | met |
| T5.4 | daemon memo half <= 5 us per leaf on a warm `get` | `get p.cached` (memo) vs `get p.plain` (template only), 5000 interleaved rounds, one connection, load 105: p50 +14 us, p90 +65 us, min +3.4 us. No `validity.rs` case was built; this is a Python socket client, so it includes the larger response line | missed at p50 (noise at this load is the same order); min is within |
| T5.5 | after a kubeconfig rewrite the next `get` renders afresh 100/100; first read <= 1 ms over a hit plus the spawn | 100/100 by count: `shipped::kubecontext::kubecontext_runs_again_when_kubeconfig_changes` and `exec_cache::a_change_to_a_watched_file_runs_it_again`. Latency: the rewrite-then-read median equals the baseline's uncached read within 0.2 ms, which is a hit plus a spawn; `prompt_race.rs` has no memo case | count met; latency consistent with the target, not resolved below load noise |
| T5.6 | a miss exits <= 1 ms after its output | not measured separately: no case built. The miss above is no slower than an uncached read, so the write-back adds under the run-to-run noise (several ms at this load) | not shown |
| T5.7 | memo record <= 128 B plus value and paths | measured by allocation over 2000 memos in one provider: **635 B** for a memo of absence (hash only), **1403 B** with one watch path and a ttl, **1578 B** with a value `ctx-1` too. A memo is stored as generic `Value` maps in two facets and a stamp map, not a packed struct | **missed**, about 5-12 times the target |
| T5.8 | suite about 10 s; each new test under 0.1 s | full workspace, 2486 tests, 25.4 s wall at load about 110 (the idle figure is not measurable now). The new `shipped::kubecontext` tests take 0.47-0.58 s each, 1.1 s for the `comb status` one, at the same load; their cost is two or three `comb` processes | not judgeable at this load |

The library's `resolve` benches (`cargo bench --bench resolve`) were taken at load about 47, alternating: `env_read`
46 us (1 variable) / 131 us (140), `memo_hit` 62 / 152 us, `exec` 5.36 / 14.0 ms. A hyperfine of the CLI at load 32-47:
a hit 6.1 ms median against 95.5 ms for a render and 4.4 ms for `hostname.name`.

Close-out: `cargo xtask gen-header --check` clean (no ABI change), `scripts/conformance-all.sh` passes every SDK runner
(Lua 45 pass, 11 skip, as before), `cargo bench --bench resolve -- --test` runs `memo_hit`.

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
cargo bench --bench resolve       # the client-side resolver, no daemon
cargo bench --bench validity      # warm reads, and the reads after a cold run (P9)
cargo bench --bench watch         # the watch fan-in: delivery, open, cancel, deadline, idle cost
cargo bench --bench prompt_race   # needs a host whose native watcher delivers
cargo bench --bench load          # a configuration loaded from text: the shipped files, and with scanline's drop-in
cargo bench --profile dev --bench load   # the same in the debug build the test suite runs

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
- [ ] CLI invocation (`comb get` against a running daemon) stays under 6ms median (run `cargo bench --bench cli`; baseline `pre-sync` measured ~4.0ms median / ~4.35ms mean — process spawn benchmarks are noisier than in-process ones)
