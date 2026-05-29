---
sidebar_position: 2
title: Provider Development
---

# Provider Development Guide

Step-by-step reference for writing new built-in providers. Built-in providers live in `src/provider/` and are compiled into the daemon. For a lower-effort path using shell scripts, see §6.

---

## 1. The Provider/Source/Field Model

### Three layers

- **Provider** — a namespace. Declares 1+ Sources. Named by the key used in `comb get <name>.<field>`.
- **Source** — one invalidation strategy, one lifecycle, one set of fields. The unit of execution and scheduling.
- **Field** — a typed value. Belongs to exactly one Source.

Every provider implements:

```rust
pub trait Provider: Send + Sync {
    fn metadata(&self) -> ProviderMetadata;
    fn sources(&self) -> Vec<Box<dyn Source>>;
}
```

Every source implements:

```rust
pub trait Source: Send + Sync {
    fn metadata(&self) -> &SourceMetadata;
    fn execute(&self, path: Option<&str>) -> SourceResult;
    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        path.map(|s| s.to_string())  // default: pass through
    }
}
```

**`Provider::metadata()`** returns `ProviderMetadata { name, sources: Vec<SourceMetadata> }`. Called at registration time.

**`Provider::sources()`** returns one `Box<dyn Source>` per declared `SourceMetadata`. The registry validates that names and fields match the metadata declaration.

**`Source::execute(path)`** runs the source and returns `SourceResult { fields: HashMap<String, Value> }`. Called on a blocking thread pool (`tokio::task::spawn_blocking`), so it may safely call `std::process::Command`, `std::fs::read_to_string`, and other blocking operations. Return an empty `SourceResult` (empty `fields` map) to indicate no value is available — the cache sub-entry for this source is not updated.

**`Source::canonical_path(path)`** normalises the input path to a project-root. Providers that walk up to a marker (git, mise, direnv, asdf, terraform, python) override this; the default implementation passes the path through unchanged.

### SourceMetadata fields

```rust
pub struct SourceMetadata {
    pub name: String,
    pub fields: Vec<FieldSchema>,
    pub scope: SourceScope,          // Global or PathScoped
    pub invalidation: InvalidationStrategy,
    pub keep_alive: KeepAlive,
    pub failback: FailbackConfig,
    pub fsevents_reinstate: bool,    // default true for Watch / WatchAndPoll
}
```

`SourceScope::Global` — lifecycle key is `(provider, None, source)`. Watched via `abs_paths` only.
`SourceScope::PathScoped` — lifecycle key is `(provider, Some(path), source)`. Watched via relative `patterns` resolved under the scope path.

### ProviderMetadata

```rust
pub struct ProviderMetadata {
    pub name: String,
    pub sources: Vec<SourceMetadata>,
}
```

There is no `global: bool` field on `ProviderMetadata`. Scope is a per-Source property.

### FieldSchema

`FieldSchema { name, field_type }` — scope is a Source-level property, not per-field.

### KeepAlive

```rust
pub enum KeepAlive {
    Polls(u32),       // Valid for Poll and WatchAndPoll. Entry stays Active for K polls.
    Duration(u64),    // Valid for Watch + PathScoped. Entry stays Active for K_secs.
    Never,            // Valid for Watch + Global. Entry never decays.
}
```

### expand_abs_path()

The `expand_abs_path(s: &str) -> Option<PathBuf>` helper (in `src/provider/mod.rs`) expands `~`, `$HOME`, `$XDG_CONFIG_HOME`, `$XDG_DATA_HOME`, `$XDG_STATE_HOME`, and `$XDG_CACHE_HOME` at metadata construction time, with XDG defaults when the env vars are unset. Sources should call this in `metadata()` so the scheduler receives canonical absolute paths.

---

## Implementing a Source — worked example: git

`git` decomposes into three sources with different strategies:

```
git.refs   → fsevent, path-scoped, watches .git/
             fields: branch, commit, tag, ahead, behind, upstream, detached, state, stash

git.diff   → poll 30s, path-scoped
             fields: lines_added, lines_removed, lines_staged_added, lines_staged_removed

git.status → fsevent_poll (watches .git/index + polls every 60s), path-scoped
             fields: staged, unstaged, untracked, conflicted, dirty
```

This decomposition allows git branch lookups (hot path, fsevent-driven) to stay fresh without dragging along the slower diff computation.

Consumers still use `comb get git.branch .` as before — the registry's `field → source` map routes the query to the `refs` source transparently.

---

## 2. Step-by-Step: Writing a New Provider

### 2.1 Decide on sources

Before writing code, answer these questions for each logical group of fields:

1. How do these fields change? (filesystem event, timer, or both)
2. What is the scope? (global: one instance total; path-scoped: one instance per project root)
3. Do I need absolute-path watches? (e.g., watching `~/.config/mise/config.toml`)

Use this decision table:

| Signal | Scope | Use |
|---|---|---|
| File changes only | path or global | `fsevent` |
| Timer only | path or global | `poll` |
| File changes + safety timer | path or global | `fsevent_poll` |
| Truly static (hostname, uname, user) | global | `fsevent` + `KeepAlive::Never` (pure-watch global — no decay) |
| Global config file watch (mise.global, mise config dir) | global | `fsevent` + `abs_paths` + `KeepAlive::Never` |

**Pure-watch global** sources use `Watch` strategy with `KeepAlive::Never` — no decay timer, no polling. They execute once on first demand and only re-execute when an fs event fires on their registered `abs_paths`. Suitable for values that can only change when a specific file is written: hostname (requires reboot), username (requires reboot), uname (requires reboot), mise global config.

### 2.2 Create the file

Create `src/provider/dockercontext.rs`:

```rust
use crate::provider::{
    expand_abs_path, FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive,
    Provider, ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope,
};
use std::path::PathBuf;

// ── Provider ──────────────────────────────────────────────────────────────────

pub struct DockerContextProvider;

impl Provider for DockerContextProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "dockercontext".to_string(),
            sources: vec![DockerContextSource.metadata().clone()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(DockerContextSource)]
    }
}

// ── Source ────────────────────────────────────────────────────────────────────

struct DockerContextSource;

impl Source for DockerContextSource {
    fn metadata(&self) -> &SourceMetadata {
        static META: std::sync::OnceLock<SourceMetadata> = std::sync::OnceLock::new();
        META.get_or_init(|| SourceMetadata {
            name: "config".to_string(),
            fields: vec![
                FieldSchema { name: "name".to_string(), field_type: FieldType::String },
                FieldSchema { name: "endpoint".to_string(), field_type: FieldType::String },
            ],
            scope: SourceScope::Global,
            invalidation: InvalidationStrategy::Watch {
                patterns: vec![],
                abs_paths: vec![
                    expand_abs_path("~/.docker/config.json")
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    expand_abs_path("~/.docker/contexts")
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ],
            },
            keep_alive: KeepAlive::Never,
            failback: FailbackConfig { reattempts: 3, interval_secs: 60 },
            fsevents_reinstate: true,
        })
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let home = match std::env::var("HOME").ok() {
            Some(h) => h,
            None => return SourceResult::default(),
        };
        let config_path = PathBuf::from(&home).join(".docker").join("config.json");
        let config_text = match std::fs::read_to_string(&config_path).ok() {
            Some(t) => t,
            None => return SourceResult::default(),
        };
        let config: serde_json::Value = match serde_json::from_str(&config_text).ok() {
            Some(v) => v,
            None => return SourceResult::default(),
        };
        let context_name = config
            .get("currentContext")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();
        let endpoint = read_context_endpoint(&home, &context_name)
            .unwrap_or_else(|| "unix:///var/run/docker.sock".to_string());

        let mut result = SourceResult::new();
        result.insert("name".to_string(), crate::provider::Value::String(context_name));
        result.insert("endpoint".to_string(), crate::provider::Value::String(endpoint));
        result
    }
}

fn read_context_endpoint(home: &str, context_name: &str) -> Option<String> {
    if context_name == "default" {
        return None;
    }
    let meta_dir = PathBuf::from(home).join(".docker").join("contexts").join("meta");
    for entry in std::fs::read_dir(&meta_dir).ok()? {
        let entry = entry.ok()?;
        let meta_path = entry.path().join("meta.json");
        let text = std::fs::read_to_string(&meta_path).ok()?;
        let meta: serde_json::Value = serde_json::from_str(&text).ok()?;
        if meta.get("Name").and_then(|v| v.as_str()) == Some(context_name) {
            return meta
                .pointer("/Endpoints/docker/Host")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
    }
    None
}
```

### 2.3 Register the provider

Add the module to `src/provider/mod.rs`:

```rust
pub mod dockercontext;
```

Add the import and registration to `src/provider/registry.rs`:

```rust
use crate::provider::dockercontext::DockerContextProvider;

// In with_defaults():
("dockercontext", Box::new(DockerContextProvider)),
```

### 2.4 Use it

```bash
comb get dockercontext.name
comb get dockercontext.endpoint
# Source-level addressing:
comb get dockercontext.config
```

### 2.5 Write a test

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_valid() {
        let provider = DockerContextProvider;
        let meta = provider.metadata();
        assert_eq!(meta.name, "dockercontext");
        assert_eq!(meta.sources.len(), 1);
        let src = &meta.sources[0];
        assert_eq!(src.name, "config");
        assert!(src.fields.iter().any(|f| f.name == "name"));
        assert!(src.fields.iter().any(|f| f.name == "endpoint"));
        assert_eq!(src.scope, SourceScope::Global);
        assert!(matches!(src.keep_alive, KeepAlive::Never));
    }

    #[test]
    fn returns_empty_without_docker_config() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", dir.path());
        let source = DockerContextSource;
        let result = source.execute(None);
        assert!(result.fields.is_empty());
        std::env::remove_var("HOME");
    }

    #[test]
    fn reads_default_context() {
        let dir = tempfile::tempdir().unwrap();
        let docker_dir = dir.path().join(".docker");
        std::fs::create_dir_all(&docker_dir).unwrap();
        std::fs::write(
            docker_dir.join("config.json"),
            r#"{"auths": {}, "currentContext": "default"}"#,
        ).unwrap();

        std::env::set_var("HOME", dir.path());
        let source = DockerContextSource;
        let result = source.execute(None);
        assert_eq!(
            result.fields.get("name"),
            Some(&crate::provider::Value::String("default".to_string()))
        );
        std::env::remove_var("HOME");
    }
}
```

Run with:

```bash
cargo nextest run -p beachcomber -E 'test(provider::dockercontext)'
```

---

## 3. InvalidationStrategy: Choosing the Right Variant

```rust
pub enum InvalidationStrategy {
    Poll { interval_secs: u64 },
    Watch { patterns: Vec<String>, abs_paths: Vec<String> },
    WatchAndPoll { patterns: Vec<String>, abs_paths: Vec<String>, interval_secs: u64 },
}
```

There is no `Once` variant. Values that were previously declared `Once` (hostname, user, uname) are now pure-watch globals: `Watch { patterns: [], abs_paths: [...] }` with `KeepAlive::Never`.

Strategy chooser:

| Signals available | Use |
|---|---|
| Timer only, no watchable file | `Poll` |
| Filesystem events only, no poll needed | `Watch` |
| Filesystem events + safety backstop timer | `WatchAndPoll` |
| Truly static value (hostname, uname, user) | `Watch` (empty patterns + abs_paths, `KeepAlive::Never`) |
| Global config dir watch | `Watch` (abs_paths only, `KeepAlive::Never`) |

**`Poll { interval_secs }`** — re-execute on a timer. Use when there is no file to watch that reliably reflects state changes.

```rust
// battery level: no file to watch reliably, poll every 30s
invalidation: InvalidationStrategy::Poll { interval_secs: 30 },
```

**`Watch { patterns, abs_paths }`** — re-execute when filesystem paths change. `patterns` are relative path components matched within the source's scope path (path-scoped sources only). `abs_paths` are absolute roots watched directly (typically used by global sources or for cross-repo config files). Use `expand_abs_path()` to expand `~`, `$HOME`, and XDG vars at metadata construction time.

```rust
// git.refs: re-run on any change under .git/ (path-scoped)
invalidation: InvalidationStrategy::Watch {
    patterns: vec![".git".to_string()],
    abs_paths: vec![],
},
// mise.global: re-run when the global mise config dir changes
invalidation: InvalidationStrategy::Watch {
    patterns: vec![],
    abs_paths: vec![
        expand_abs_path("$XDG_CONFIG_HOME/mise")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    ],
},
```

There is no automatic poll fallback for `Watch` sources. If filesystem watching registration fails at runtime, the scheduler logs a warning and the source serves its last cached value. If a fallback backstop is needed, use `WatchAndPoll` instead.

**`WatchAndPoll { patterns, abs_paths, interval_secs }`** — watch files AND poll on a timer. Use when file watching catches most changes quickly but some changes don't touch a watchable file (e.g., network-propagated git changes via `git fetch`).

```rust
// git.status: watch .git/index for staging changes, poll every 60s for remote drift
invalidation: InvalidationStrategy::WatchAndPoll {
    patterns: vec![".git/index".to_string()],
    abs_paths: vec![],
    interval_secs: 60,
},
```

For path-scoped sources, `patterns` like `".git"` are relative to the queried path. The scheduler resolves them to absolute watch roots when demand is first registered.

---

## 3b. Cache Lifecycle Tuning

By default, providers inherit global lifecycle settings. Per-source overrides live in `[providers.<name>.<source>]` using these TOML keys (from `SourceOverrideConfig` in `src/config.rs`):

| TOML key | Type | Description |
|---|---|---|
| `poll_interval` | duration string | Base poll rate for this source while Active. |
| `poll_count` | u32 | Number of Active polls that count as "alive" before decay begins. |
| `fsevent_patterns` | array of strings | Relative patterns to watch within scope path. |
| `fsevent_abs_paths` | array of strings | Absolute paths to watch. |
| `fsevent_lifespan` | duration string | Keep-alive duration for Watch sources. |
| `fsevent_reinstates` | bool | Whether watches survive decay and reinstate to Active on fs events. |

Example:

```toml
[providers.git.refs]
fsevent_reinstates = true

[providers.git.diff]
poll_interval = "30s"
poll_count = 4
```

The full decay model — state machine, step durations, reinstatement rules — is specified in `docs/cache-lifecycle.md`.

---

## 4. Performance Guidelines

Provider execution happens on tokio's blocking thread pool. Slow providers delay cache freshness but do not block the scheduler loop. Still, keep providers fast. The tier list from `docs/performance.md`:

| Tier | Target | Method |
|---|---|---|
| Nanosecond (`<1µs`) | `user`, `hostname`, `kubecontext`, `gcloud`, `aws` | libc calls, env vars, file reads + line scan |
| Microsecond (1-100µs) | `terraform`, `python`, `direnv` (no binary) | File existence checks + small reads |
| Millisecond (1-10ms) | `git`, `network`, `battery` | At most one process spawn |
| Slow (10-50ms) | `mise`, `direnv` (with binary), script providers | Multiple spawns or interpreted CLI |

**Rule 1: Never fork a process when you can read a file.**

Process spawns cost 2-6ms minimum. File reads cost nanoseconds. Before using `Command::new(...)`, ask: does this tool write its state to a file I can parse?

```rust
// Bad: 5ms to spawn git just to count stashes
let output = Command::new("git").args(["stash", "list"]).output().ok()?;
let count = output.stdout.lines().count();

// Good: ~1µs to read the stash log file directly
let stash_log = dir.join(".git").join("logs").join("refs").join("stash");
let count = std::fs::read_to_string(&stash_log)
    .map(|s| s.lines().count() as i64)
    .unwrap_or(0);
```

Real examples from `docs/performance.md`:
- `gcloud`: reading `~/.config/gcloud/properties` instead of spawning the Python CLI — 500ms to 1µs (~500,000x)
- `kubecontext`: reading `~/.kube/config` instead of running `kubectl` — 60ms to 749ns (~80,000x)
- `git` stash: reading `.git/logs/refs/stash` instead of `git stash list` — 5ms to 1µs

**Rule 2: If you must spawn a process, spawn exactly one.**

If a file read is truly not feasible, cap the provider at one process spawn. The `git` provider spawns one (`git status`). The `network` provider spawns one (`airport` for SSID; everything else uses `libc::getifaddrs()`).

**Rule 3: Providers that poll frequently must be fast.**

A provider polling every 5 seconds and taking 50ms per execution consumes 1% of a blocking thread slot continuously. Use `Poll { interval_secs }` values that match the provider's actual cost:
- Sub-microsecond providers: can poll every 5-10s safely
- Millisecond providers: 30s minimum
- Slow providers (>10ms): 60s minimum or use `Watch` instead

**Rule 4: Sources must be stateless.**

`execute()` receives no mutable state. Do not use `Mutex`-wrapped fields inside your source struct to cache intermediate results — this adds contention and complexity. If two concurrent calls to `execute()` are needed (different paths), they must be independent.

See `docs/performance.md` for the full performance profile, benchmark commands, and the regression checklist.

---

## 5. Testing Patterns

**Basic structure**

Every provider file should have a `#[cfg(test)]` module. At minimum, test:
1. `metadata()` returns valid, expected values — check `sources.len()`, source names, field names, scope, `keep_alive` variant
2. `source.execute()` returns empty `SourceResult` when the required tool/file is absent
3. `source.execute()` returns the expected fields when given a valid fixture

**Using tempdir**

For sources that read files, use `tempfile::tempdir()` to create a controlled environment:

```rust
#[test]
fn detects_git_repo() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    let provider = GitProvider;
    let sources = provider.sources();
    let refs_source = sources.iter().find(|s| s.metadata().name == "refs").unwrap();
    // Should not panic; may return empty result for a bare .git
    let _ = refs_source.execute(Some(dir.path().to_str().unwrap()));
}
```

**Testing with real git repos**

For sources that shell out (like `git`), test against a real initialized repo:

```rust
#[test]
fn git_refs_on_empty_repo() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .unwrap();

    let provider = GitProvider;
    let sources = provider.sources();
    let refs_source = sources.iter().find(|s| s.metadata().name == "refs").unwrap();
    let result = refs_source.execute(Some(dir.path().to_str().unwrap()));
    assert_eq!(
        result.fields.get("branch"),
        Some(&crate::provider::Value::String("main".to_string()))
    );
}
```

**Testing when the external tool is not installed**

Sources that depend on optional tools must return an empty `SourceResult` gracefully when the tool is absent or when the relevant config files do not exist:

```rust
#[test]
fn returns_empty_without_kubeconfig() {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", dir.path());
    std::env::remove_var("KUBECONFIG");

    let provider = KubecontextProvider;
    let sources = provider.sources();
    let result = sources[0].execute(None);
    assert!(result.fields.is_empty());

    std::env::remove_var("HOME");
}
```

Avoid `std::env::set_var` in parallel tests — it mutates global state. cargo-nextest runs each test in its own process by default, so env-var mutations are isolated per test; `--test-threads=1` is only needed for plain `cargo test`.

**Testing `metadata()` completeness**

A quick structural test catches registration bugs early:

```rust
#[test]
fn metadata_sources_match_sources_fn() {
    let provider = DockerContextProvider;
    let meta = provider.metadata();
    let sources = provider.sources();
    assert_eq!(meta.sources.len(), sources.len());
    for (m, s) in meta.sources.iter().zip(sources.iter()) {
        assert_eq!(m.name, s.metadata().name);
        for field in &m.fields {
            // Each declared field name should appear in a real execute() result
            // when given a valid fixture. This is a smoke check, not exhaustive.
            let _ = field.name.as_str(); // at minimum, names are non-empty
        }
    }
}
```

---

## 6. Script Providers vs Built-in Providers

### When to use a script provider

Script providers are defined in `~/.config/beachcomber/config.toml` without writing any Rust. Use them when:

- The logic is simple or already exists as a shell script
- The tool does not have a file-based state representation (forced to shell out)
- The data changes infrequently so the performance cost is acceptable
- You need something working today and can write a built-in later

### How script providers work

A script provider entry in config:

```toml
[providers.my_vpn]
command = "vpn-status --json"
output = "json"

[providers.my_vpn.invalidation]
poll = "10s"
watch = ["/etc/vpn/state"]
```

This creates a `ScriptProvider` instance (see `src/provider/script.rs`) that:
1. Runs `sh -c "vpn-status --json"` when executed
2. Parses stdout as JSON (field: `output = "json"`) or key=value pairs (`output = "kv"`)
3. Returns the parsed fields as a `SourceResult`

The `invalidation` config maps directly to `InvalidationStrategy`:
- `poll` only -> `Poll { interval_secs }`
- `watch` only -> `Watch { patterns, abs_paths: [] }`
- Both -> `WatchAndPoll { patterns, abs_paths: [], interval_secs }`
- Neither -> `Poll { interval_secs: 30 }` (default)

Set `scope = "path"` to make the provider path-scoped (the script will be run with its working directory set to the queried path):

```toml
[providers.project_version]
command = "cat VERSION 2>/dev/null || echo unknown"
output = "kv"   # stdout format: "version=1.2.3"
scope = "path"

[providers.project_version.invalidation]
watch = ["."]
```

### When to write a built-in

Prefer a built-in provider when:

- **Performance matters**: The provider will be queried frequently (prompt, tmux, status bar) and `spawn_blocking` a process every 5-30s adds up
- **File parsing is required**: The tool stores state in a structured file (INI, TOML, plain text) that you can parse directly without spawning the tool
- **Cross-platform behaviour**: Shell semantics differ between sh and cmd.exe; Rust handles this uniformly
- **The provider will be broadly useful**: If most beachcomber users would want it, it belongs in the binary

The perf breakeven point: if direct file reading brings execution from `>1ms` to `<100µs`, write a built-in. If the tool must be shelled out anyway and the data changes slowly, a script provider is fine.

### Migrating a script provider to built-in

1. Identify what the script does — which file does it read, or which binary does it call?
2. Check `docs/performance.md` to see if the tool has already been handled as a file read
3. Write the built-in following §2 above, matching the field names your existing config consumers expect
4. Remove the script entry from config and register the built-in in `registry.rs`
5. Run `cargo bench --bench providers` before and after to verify the improvement

---

## 7. Platform-Specific Providers

Some providers have different implementations per platform. Two patterns:

**Inline `#[cfg]` (simple cases):** When the platform difference is one or two functions, use `#[cfg(target_os = "...")]` on the differing functions within a single file. See `battery.rs` for an example — shared `BatteryProvider` struct with platform-specific `execute_platform()` functions.

**Submodule split (complex cases):** When platform differences are substantial, split into `provider_name/mod.rs` (shared), `provider_name/macos.rs`, `provider_name/linux.rs`. See `network/` for an example. The platform modules expose `pub(crate)` functions called from `mod.rs` via `#[cfg]` dispatch.

For testing, gate platform-specific tests with `#[cfg(target_os = "...")]`. Make parse functions `pub(crate)` so they can be unit tested from `tests/` without exposing them publicly.

---

## 8. Shared Library Providers

Shared library providers load a `.so` (Linux) or `.dylib` (macOS) at daemon startup and call into it via a C ABI. This avoids the per-execution process spawn cost of script providers while allowing provider logic to be written in any language that can produce a shared library with C-compatible exports.

### C ABI — Two ABIs

`src/provider/library.rs` detects which ABI the library exports by probing for `bc_source_count`:

**Multi-source ABI (current):** Three symbols, called per source index:

```c
// Returns the number of sources this library exports.
uintptr_t bc_source_count(void);

// Returns JSON SourceMetadata for source at index `idx`.
// Caller frees via beachcomber_provider_free().
const char* bc_source_metadata(uintptr_t idx);

// Execute source `idx`. path is NULL for global sources.
// Returns a JSON object of field values, or NULL on failure.
// Caller frees via beachcomber_provider_free().
const char* bc_source_execute(uintptr_t idx, const char* path);
```

**Legacy ABI (backwards-compatible):** Used when `bc_source_count` is absent:

```c
// Returns JSON metadata for the single source.
// Caller frees via beachcomber_provider_free().
const char* beachcomber_provider_metadata(void);

// Execute the single source. path is NULL for global sources.
// Caller frees via beachcomber_provider_free().
const char* beachcomber_provider_execute(const char* path);

// Free a string previously returned by metadata or execute.
void beachcomber_provider_free(char* ptr);
```

Both ABIs use the same free symbol: `beachcomber_provider_free`.

### Configuration

```toml
[providers.my_native_provider]
type = "library"
library_path = "/usr/local/lib/beachcomber/libmy_provider.so"

# Optional: override metadata from config instead of the library
# scope = "path"
# fields = { "field1" = "string", "field2" = "int" }
# invalidation = { poll = "30s" }
```

### When to Use

- **Performance-critical providers** where even a 2-6ms process spawn is too expensive
- **Complex logic** that benefits from compiled code but doesn't belong in the daemon binary
- **Third-party plugins** distributed as shared libraries
- **Language flexibility** — write providers in C, C++, Rust, Go (with cgo), Zig, or any language that can export C symbols

### Example: Minimal C Provider (multi-source ABI)

```c
#include <stdlib.h>
#include <string.h>
#include <stddef.h>

uintptr_t bc_source_count(void) { return 1; }

const char* bc_source_metadata(uintptr_t idx) {
    (void)idx;
    return strdup("{\"name\":\"main\","
                  "\"fields\":{\"value\":{\"type\":\"string\"}},"
                  "\"invalidation\":{\"poll\":\"10s\"}}");
}

const char* bc_source_execute(uintptr_t idx, const char* path) {
    (void)idx; (void)path;
    return strdup("{\"value\":\"hello from C\"}");
}

void beachcomber_provider_free(char* ptr) { free(ptr); }
```

Build: `cc -shared -o libhello.so hello_provider.c` (Linux) or `cc -shared -o libhello.dylib hello_provider.c` (macOS).

Configure:
```toml
[providers.hello]
type = "library"
library_path = "/path/to/libhello.so"
```

Test: `comb get hello.value` → `hello from C`
