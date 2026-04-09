---
sidebar_position: 1
title: Architecture
---

# beachcomber Architecture

Internal reference for contributors. Describes how the daemon (`comb daemon`) is structured, how components relate, and the reasoning behind key design choices.

---

## 1. System Overview

```
  CLI / consumer
      |
      | Unix socket (newline-delimited JSON)
      v
  +-------------------+
  |      Server       |  one tokio task per connection
  |  (server.rs)      |  accepts, reads requests, writes responses
  +-------------------+
      |          |
      | cache    | scheduler messages (mpsc)
      v          v
  +-------+   +-----------+
  | Cache |   | Scheduler |  single async event loop
  | (DashMap)  | (scheduler.rs)
  +-------+   +-----------+
                  |          \
          spawn_blocking   FsWatcher
          (thread pool)    (notify crate)
                  |
          Arc<dyn Provider>
          executes, returns ProviderResult
                  |
                  v
              Cache.put()
```

Data flow summary:

- **Read path**: client -> Server -> Cache.get() -> response (no provider involved)
- **Write path**: trigger (fs event or poll timer) -> Scheduler -> spawn_blocking(provider.execute()) -> Cache.put()
- **Miss path**: client asks for uncached key -> Server sends Poke to Scheduler -> Scheduler fires provider -> client retries or polls

The Server and Scheduler share `Arc<Cache>` and `Arc<ProviderRegistry>`. The Server sends messages to the Scheduler over an `mpsc::Sender<SchedulerMessage>`. The Scheduler owns the `FsWatcher` and all demand/poll state.

---

## 2. Module Map

| File | Responsibility |
|---|---|
| `src/lib.rs` | Module declarations; no logic |
| `src/daemon.rs` | Process lifecycle: fork, pid file, wait-for-socket, `run_daemon_with_cancel` |
| `src/server.rs` | Unix socket accept loop; one task per connection; request dispatch; response formatting |
| `src/scheduler.rs` | Single event loop: handles Poke/FsEvent/QueryActivity/poll tick; owns all demand and watch state; contains `BackoffState`/`BackoffStage` |
| `src/cache.rs` | Lock-free DashMap store mapping `"provider\0path"` keys to `CacheEntry`; generation counter |
| `src/watcher.rs` | Thin wrapper around the `notify` crate; exposes `watch(path)` / `unwatch(path)` and an mpsc receiver of path events |
| `src/protocol.rs` | Serde types for the wire protocol: `Request`, `Response`, `Format`; `split_key()` |
| `src/config.rs` | TOML config loading from XDG directories; `Config`, `DaemonConfig`, `LifecycleConfig`, `ScriptProviderConfig` |
| `src/provider/mod.rs` | `Provider` trait; `ProviderResult`, `Value`, `ProviderMetadata`, `FieldSchema`, `InvalidationStrategy` |
| `src/provider/registry.rs` | `ProviderRegistry`: `HashMap<String, Arc<dyn Provider>>`; built-in registration; script provider registration from config |
| `src/watcher_registry.rs` | Broadcast channel registry for watch subscribers; notified by cache on every put |
| `src/provider/hostname.rs` | `HostnameProvider`: libc `gethostname`, `Once` strategy, global scope |
| `src/provider/git.rs` | `GitProvider`: `git status --porcelain=v2`, file reads for stash/state; `WatchAndPoll` on `.git` |
| `src/provider/script.rs` | `ScriptProvider`: runs arbitrary shell commands; parses JSON or KV output; strategy built from config |
| `src/client.rs` | `Client` (one-shot) and `ClientSession` (persistent) for consumer-side socket communication |

The remaining provider files (`battery`, `load`, `uptime`, `network`, `kubecontext`, `aws`, `gcloud`, `terraform`, `direnv`, `python`, `conda`, `mise`, `asdf`) follow the same pattern as `git.rs` — each implements `Provider` for a specific domain.

---

## 3. Request Lifecycle: `comb get git.branch . -f text`

This traces the full path from CLI invocation to output on stdout.

**Step 1: CLI entry point**

The `comb get` subcommand resolves the socket path (XDG runtime dir or `$TMPDIR/beachcomber-$UID/sock`). If no socket exists, it calls `daemon::ensure_daemon()` which forks `comb daemon --socket <path>` as a detached child and waits up to ~1.5s for the socket to appear.

**Step 2: Socket connection**

The client opens a Unix stream to the socket. For a one-shot `comb get`, this is a fresh connection. For `ClientSession` consumers (prompts, status bars), the connection is reused.

**Step 3: Request serialisation**

The CLI constructs:
```json
{"op":"get","key":"git.branch","path":".","format":"text"}
```
and writes it as a single newline-terminated line.

**Step 4: Server receives the request**

`Server::run()` has accepted the connection and spawned `handle_connection()` as a tokio task. The task reads lines from a `BufReader`, deserialises into `Request::Get { key: "git.branch", path: Some("."), format: Text }`.

**Step 5: Key splitting**

`protocol::split_key("git.branch")` returns `("git", Some("branch"))`. The provider name is `"git"`, the field name is `"branch"`.

**Step 6: Path resolution**

`resolve_path(Some("."), &context_path, "git", &registry)` checks whether the `git` provider is path-scoped (`global: false`). It is, so the explicit path `"."` is used. (If the CLI had omitted the path argument, the session's context path would be used instead.)

**Step 7: Cache lookup**

`cache.get("git", Some("."))` constructs the key `"git\0."` and looks it up in the DashMap. On a cache hit, it returns `Some(CacheEntry)`.

**Step 8: Field extraction**

The handler accesses `entry.result.get("branch")` and converts the `Value::String` to a `serde_json::Value`. It packages this into `Response::ok(data, age_ms, stale)`.

**Step 9: Response formatting**

`format_response(&request, &response)` sees `Format::Text`. The data is a `serde_json::Value::String`, so it outputs the raw string followed by a newline — no JSON wrapper.

**Step 10: CLI prints and exits**

The client reads the response line, strips metadata, and writes the plain text to stdout.

**On a cache miss:**

The server returns `Response::miss()` (ok=true, no data). The CLI treats this as an empty result or, in interactive use, sends a `Poke` request to trigger a fresh execution, then polls with exponential backoff until a value appears or a timeout is reached.

---

## 4. Provider Execution Lifecycle: Filesystem Event -> Cache Write

This traces what happens when the user saves a file in a git repository being watched.

**Step 1: OS delivers an inotify/kqueue event**

The `notify` crate's `RecommendedWatcher` receives the event on a background thread. It calls the closure registered in `FsWatcher::new()`, which calls `tx.blocking_send(event.paths)` on the mpsc channel.

**Step 2: Scheduler receives the event**

The scheduler's `tokio::select!` loop picks up `Some(paths) = fs_rx.recv()`. It calls `self.handle_fs_event(paths, &watch_paths)`.

**Step 3: Path matching**

`handle_fs_event` iterates over the changed paths. For each changed path, it scans `watch_paths` (a `HashMap<PathBuf, Vec<(String, Option<String>)>>`) looking for any registered watch path that is a prefix of (or equal to) the changed path. For a `.git/index` change, the watch path `".git"` matches.

**Step 4: execute_provider is called**

`self.execute_provider("git", Some("/home/user/myrepo"))` is invoked.

**Step 5: Deduplication check**

The scheduler checks `in_flight` (a `Mutex<HashSet>`). If an execution for `("git", Some("/home/user/myrepo"))` is already running, the key is inserted into `pending_rerun` and the function returns immediately. This ensures at most one in-flight execution per (provider, path) at any time, with one queued rerun if changes arrive during execution.

**Step 6: Failure backoff check**

Before launching, the scheduler checks `failure_counts`. If the provider has exceeded its failure threshold (`failure_reattempts`, default 3) and is within its exponential backoff window (starting at `failure_backoff_interval`, doubling for 4 levels), execution is skipped. Both values are configurable globally in `[lifecycle]` and per-provider in `[providers.<name>]`.

**Step 7: spawn_blocking**

The scheduler clones `Arc<dyn Provider>` from the registry and calls:
```rust
tokio::spawn(async move {
    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        tokio::task::spawn_blocking(move || provider.execute(path)),
    ).await;
    ...
});
```

`spawn_blocking` runs `provider.execute()` on tokio's dedicated blocking thread pool. This is critical: the scheduler's async loop continues processing messages, poll ticks, and more filesystem events while the provider runs.

**Step 8: Provider executes**

`GitProvider::execute(Some("/home/user/myrepo"))` runs `git status --porcelain=v2 --branch` in a subprocess and reads `.git/logs/refs/stash` directly. It returns `Some(ProviderResult)`.

**Step 9: Cache write**

Back in the async context, on success: `cache.put_with_interval("git", Some("/home/user/myrepo"), result, poll_interval_secs)`. The DashMap insert is lock-free.

**Step 10: Rerun check**

After clearing `in_flight`, the scheduler checks `pending_rerun`. If a rerun was queued (another event arrived while this execution was in progress), a new `spawn_blocking` is launched immediately for the same (provider, path).

---

## 5. Demand-Driven Cache Lifecycle

**Demand signal**

Every `get` request sends `SchedulerMessage::QueryActivity { provider, path }` to the scheduler. This records the current time in the `demand` HashMap for that key. If the key is new to demand, the scheduler immediately sets up polling (based on provider metadata) and filesystem watching (for path-scoped providers), and fires `execute_provider` if the cache is cold.

**Demand window**

The scheduler's per-second tick checks all demand entries. Any key not queried within the last 120 seconds is considered expired. When demand expires for a key:

- The poll timer for that key is removed.
- Filesystem watches for that key are removed (if no other keys share the watch path).
- A `BackoffState` is started in `Grace` stage.

**Backoff/drain sequence**

When a key enters the backoff sequence, `BackoffState` (in `src/scheduler.rs`) tracks its stage:

```
Grace (cache_lifespan, default 30s) -> SlowPoll -> Frozen -> Evict
```

At `Evict`, `cache.remove()` is called. If `QueryActivity` arrives for a key during any backoff stage, the backoff is cancelled immediately and the key re-enters the demand window.

**Idle shutdown**

When `cache.is_empty()` and `demand.is_empty()` both hold true and no activity has been seen for `lifecycle.idle_shutdown_secs`, the daemon exits.

---

## 6. Concurrency Model

**Tokio runtime**

The daemon runs on a multi-threaded tokio runtime. The Server spawns one task per connection (lightweight, async I/O only). The Scheduler runs as a single async task with a `tokio::select!` loop.

**spawn_blocking for providers**

All provider `execute()` calls happen in `tokio::task::spawn_blocking`. This moves them to a dedicated thread pool (separate from the async worker threads), ensuring that a slow provider (git, battery, network) cannot starve the scheduler loop or connection handlers. The thread pool size is managed by tokio and scales with available cores.

**DashMap for cache**

`Cache` wraps a `DashMap<String, CacheEntry>`. DashMap provides fine-grained shard locking, allowing concurrent reads and writes without a global mutex. Cache reads — the hot path hit on every client request — are effectively lock-free under contention.

**`Arc<dyn Provider>` for registry**

Providers are stored as `Arc<dyn Provider>`. When the scheduler needs to execute a provider, it calls `registry.get(name)` which returns `Arc::clone()`. The cloned Arc is moved into `spawn_blocking`. The registry itself is never mutated after startup; reads from multiple tasks are contention-free.

**Scheduler-owned mutable state**

The Scheduler owns all mutable coordination state: `demand`, `poll_states`, `watch_paths`, `backoff`. This state is only accessed from the single scheduler task, so it requires no synchronisation. The `in_flight`, `pending_rerun`, and `failure_counts` maps are wrapped in `Mutex` because they are accessed from both the scheduler task and from within `tokio::spawn` closures that run after `spawn_blocking` completes.

**No provider-side concurrency**

Providers are `Send + Sync` but stateless. They hold no mutable state. The same `Arc<dyn Provider>` can be passed to multiple concurrent `spawn_blocking` calls for different paths without coordination.

---

## 7. Key Design Decisions

**Single-string cache keys (`"provider\0path"`)**

Early versions used a `(String, Option<String>)` tuple as the DashMap key, requiring two heap allocations per lookup. Changing to a single string with a null-byte separator (`"git\0/home/user/repo"`) reduced this to one allocation and improved cache read latency by 16% (183ns to 157ns). The null byte is safe as a separator because it cannot appear in valid filesystem paths. See `docs/performance.md` §1.2.

**spawn_blocking, not async providers**

The `Provider` trait's `execute` method is synchronous (`fn execute(&self, path: Option<&str>) -> Option<ProviderResult>`). This is intentional. Providers need to call blocking APIs: `std::process::Command`, `std::fs::read_to_string`, `libc` syscalls. Async providers would require those calls to be wrapped in `spawn_blocking` internally anyway. Keeping the interface synchronous is simpler, and `spawn_blocking` at the callsite (the scheduler) is the right place to manage the thread pool boundary. This also means provider authors don't need to think about async.

**Fire-and-forget execution**

`execute_provider` returns immediately after spawning. The caller (scheduler loop or poll tick) does not wait for the result. This keeps the scheduler loop responsive at all times. Consumers learn about fresh values on their next `get` request, not via push. This is acceptable because the daemon's purpose is to maintain a low-latency cache, not to stream values.

**Deduplication via in_flight + pending_rerun**

Naively, a burst of filesystem events (e.g., a `git commit` touching many `.git` files) would launch many concurrent provider executions for the same key. The deduplication scheme allows at most one in-flight execution per key and queues at most one rerun. This means at most two executions will ever run for any key in response to a burst: one that was already in-flight when the burst began, and one that starts after it completes. This bounds both resource usage and cache write rate.

**Two independent backoff mechanisms**

Failure backoff (in `FailureState`, exponential 1s-60s, per key) suppresses execution when a provider fails repeatedly. Demand backoff (in `BackoffState`, Grace->SlowPoll->Frozen->Evict) handles resource cleanup when a key drops out of the demand window. They serve different purposes and are deliberately kept separate.

**Watch connections are long-lived**

A `watch` request takes over the connection for the duration of the stream. The server task does not return to the request dispatch loop; instead it loops, blocking on the broadcast receiver, and emits an NDJSON line to the client on each cache update for the watched key. The connection terminates when the client disconnects or the daemon shuts down.

**Field-level deduplication for watch**

Watching `git.branch` only emits a line when the branch value itself changes, not on every git provider update. The watch handler records the last emitted value and skips writes when the new value is identical. This prevents spurious output when unrelated git fields (e.g., `ahead`, `dirty`) change.

**Broadcast channels for watch subscribers**

`src/watcher_registry.rs` maintains a `tokio::sync::broadcast` channel per (provider, path) key. `Cache::put()` notifies the registry after every write. Watch connections subscribe to the relevant channel. Using broadcast rather than mpsc allows multiple simultaneous watchers on the same key without coordination between them.

**Virtual providers via store**

External processes can write arbitrary data into the cache via the `store` protocol op. The server creates a virtual provider entry — a `CacheEntry` with no associated `Arc<dyn Provider>` — and inserts it directly. Virtual providers have no `execute()` method and are never polled or evicted by the scheduler. Namespace hierarchy (builtin > script > virtual) prevents a `store` call from shadowing a real provider: if a built-in or script provider already owns the namespace, the store op is rejected.
