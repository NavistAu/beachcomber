---
sidebar_position: 2
title: Provider Development
---

# Provider Development Guide

Step-by-step reference for writing new built-in providers. Built-in providers live in `src/provider/` and are compiled into the daemon. For a lower-effort path using a config Source, see §6.

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


### expand_abs_path()

The `expand_abs_path(s: &str) -> Option<PathBuf>` helper (in `src/provider/mod.rs`) expands `~`, `$HOME`, `$XDG_CONFIG_HOME`, `$XDG_DATA_HOME`, `$XDG_STATE_HOME`, and `$XDG_CACHE_HOME` at metadata construction time, with XDG defaults when the env vars are unset. Sources should call this in `metadata()` so the scheduler receives canonical absolute paths.

---

## Implementing a Source — worked example: git

`git` decomposes into five sources with different strategies:

```
git.head    → fsevent, path-scoped, watches .git/ (pure file read of HEAD, no subprocess)
              fields: branch, detached

git.refs    → fsevent, path-scoped, watches .git/
              fields: commit, tag, ahead, behind, upstream, state, state_step, state_total,
                      stash, last_commit_age_secs, commit_summary, push_ahead, push_behind

git.diff    → poll 30s, path-scoped
              fields: lines_added, lines_removed, lines_staged_added, lines_staged_removed

git.lineage → poll 60s, path-scoped
              fields: branch_parent, lines_added_parent, lines_removed_parent,
                      branch_default, lines_added_default, lines_removed_default

git.status  → fsevent_poll (watches .git/index + polls every 60s), path-scoped
              fields: staged, unstaged, untracked, conflicted, dirty
```

This decomposition allows git branch lookups (hot path, fsevent-driven) to stay fresh without dragging along the slower diff computation.

Consumers still use `comb get git.branch .` as before — the registry's `field → source` map routes the query to the `head` source transparently.

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
| Truly static (hostname, uname, user) | global | `fsevent` (pure-watch global) |
| Global config file watch (mise.global, mise config dir) | global | `fsevent` + `abs_paths` |

**Pure-watch global** sources use the `Watch` strategy: no poll of their own. They execute once on first demand and only re-execute when an fs event fires on their registered `abs_paths`. Like every Source they decay once nothing reads them — a watch-only Source's Active window is the default poll lifespan, 60 s × 12 (12 minutes after its last read), after which its watches are dropped — and the next read runs them again. Suitable for values that can only change when a specific file is written: hostname (requires reboot), username (requires reboot), uname (requires reboot), mise global config.

### 2.2 Create the file

Create `src/provider/dockercontext.rs`:

```rust
use crate::provider::{
    expand_abs_path, FailbackConfig, FieldSchema, FieldType, InvalidationStrategy,
    Provider, ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope,
};
use std::path::PathBuf;

// ── SourceMetadata constructor ────────────────────────────────────────────────

fn config_meta() -> SourceMetadata {
    SourceMetadata {
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
        failback: FailbackConfig { reattempts: 3, interval_secs: 60 },
        fsevents_reinstate: true,
    }
}

// ── Provider ──────────────────────────────────────────────────────────────────

pub struct DockerContextProvider;

impl Provider for DockerContextProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "dockercontext".to_string(),
            sources: vec![config_meta()],
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
        META.get_or_init(config_meta)
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
// In with_defaults():
registry
    .register(Box::new(crate::provider::dockercontext::DockerContextProvider))
    .expect("dockercontext");
```

### 2.4 Use it

```bash
comb get dockercontext.name
comb get dockercontext.endpoint
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
    }

    #[test]
    fn returns_empty_without_docker_config() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: test-only env mutation; nextest runs each test in its own process
        unsafe { std::env::set_var("HOME", dir.path()); }
        let source = DockerContextSource;
        let result = source.execute(None);
        assert!(result.fields.is_empty());
        unsafe { std::env::remove_var("HOME"); }
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

        // SAFETY: test-only env mutation; nextest runs each test in its own process
        unsafe { std::env::set_var("HOME", dir.path()); }
        let source = DockerContextSource;
        let result = source.execute(None);
        assert_eq!(
            result.fields.get("name"),
            Some(&crate::provider::Value::String("default".to_string()))
        );
        unsafe { std::env::remove_var("HOME"); }
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
    Poll { interval_secs: u64, count: u32 },
    Watch { patterns: Vec<String>, abs_paths: Vec<String> },
    WatchAndPoll { patterns: Vec<String>, abs_paths: Vec<String>, interval_secs: u64, count: u32 },
}
```

There is no `Once` variant. Values that were previously declared `Once` (hostname, user, uname) are now pure-watch globals: `Watch { patterns: [], abs_paths: [...] }`; they decay like any other Source.

Strategy chooser:

| Signals available | Use |
|---|---|
| Timer only, no watchable file | `Poll` |
| Filesystem events only, no poll needed | `Watch` |
| Filesystem events + safety backstop timer | `WatchAndPoll` |
| Truly static value (hostname, uname, user) | `Watch` (empty patterns + abs_paths) |
| Global config dir watch | `Watch` (abs_paths only) |

**`Poll { interval_secs, count }`** — re-execute on a timer; the entry stays Active for `count` polls (`interval_secs × count`) after its last read. Use when there is no file to watch that reliably reflects state changes.

```rust
// battery level: no file to watch reliably, poll every 30s
invalidation: InvalidationStrategy::Poll { interval_secs: 30, count: 4 },
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

**`WatchAndPoll { patterns, abs_paths, interval_secs, count }`** — watch files AND poll on a timer. Use when file watching catches most changes quickly but some changes don't touch a watchable file (e.g., network-propagated git changes via `git fetch`).

```rust
// git.status: watch .git/index for staging changes, poll every 60s for remote drift
invalidation: InvalidationStrategy::WatchAndPoll {
    patterns: vec![".git/index".to_string()],
    abs_paths: vec![],
    interval_secs: 60,
    count: 12,
},
```

For path-scoped sources, `patterns` like `".git"` are relative to the queried path. The scheduler resolves them to absolute watch roots when demand is first registered.

---

## 3b. Cache Lifecycle Tuning

A built-in Source's failback is its own, set in code. The only per-source override of a built-in Source is its failback, in `[providers.<name>.<source>]`; any other key there, and any key but `enabled` directly under `[providers.<name>]`, is a config error.

| Key | Type | Description |
|---|---|---|
| `failback_count` | u32 | Consecutive failures before suppression. |
| `failback_interval` | duration | Initial suppression duration. |

Example:

```toml
[providers.git.status]
failback_count = 5
failback_interval = "2s"
```

A config Source sets its own schedule on its `return` line (`poll`, `watch`, `ttl`, `failback`, `budget`).

The full decay model — state machine, step durations, reinstatement rules — is described in the [architecture page](architecture.md) §5.

**Decay depth is data.** After the Active window an entry steps through its `decay` steps (4 unless told otherwise; step n lasts `lifespan × 2^n`) and is then evicted. A depth is put like any value: `comb put '<provider>:cache._.decay' 6` for one provider, `comb put '*:cache._.decay' 3` for every provider, with `--path` to scope it to a directory and below; the nearest prototype wins, and a lifecycle fixes its depth when it registers. `comb get <provider>:cache.<field>` shows the `poll`, `watch` and `decay` an entry registered with. There is no per-source keep-alive duration: `fsevent_lifespan` was removed, and a watch-only Source's Active window is the default poll lifespan (60 s × 12).

---

## 4. Performance Guidelines

Provider execution happens on tokio's blocking thread pool. Slow providers delay cache freshness but do not block the scheduler loop. Still, keep providers fast. The tier list from `docs/performance.md`:

| Tier | Target | Method |
|---|---|---|
| Nanosecond (`<1µs`) | `user`, `hostname`, `gcloud`, `aws` | libc calls, env vars, file reads + line scan |
| Microsecond (1-100µs) | `terraform`, `python`, `direnv` (no binary) | File existence checks + small reads |
| Millisecond (1-10ms) | `git`, `network`, `battery` | At most one process spawn |
| Slow (10-50ms) | `mise`, `direnv` (with binary), config Sources that shell out | Multiple spawns or interpreted CLI |

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
- `git` stash: reading `.git/logs/refs/stash` instead of `git stash list` — 5ms to 1µs

**Rule 2: If you must spawn a process, spawn exactly one.**

If a file read is truly not feasible, cap the provider at one process spawn. The `git` provider spawns one (`git status`). The `network` provider spawns one (`airport` for SSID; everything else uses `libc::getifaddrs()`).

**Rule 3: Providers that poll frequently must be fast.**

A provider polling every 5 seconds and taking 50ms per execution consumes 1% of a blocking thread slot continuously. Use `Poll { interval_secs, count }` values that match the provider's actual cost:
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
1. `metadata()` returns valid, expected values — check `sources.len()`, source names, field names, scope, invalidation variant
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

Sources that depend on optional tools or files must return an empty `SourceResult` gracefully when the tool is absent or when the relevant files do not exist — `terraform`'s source reads `.terraform/environment` and answers nothing without a `.terraform/` directory:

```rust
#[test]
fn returns_empty_without_terraform_dir() {
    let dir = tempfile::tempdir().unwrap();

    let provider = TerraformProvider;
    let sources = provider.sources();
    let result = sources[0].execute(Some(dir.path().to_str().unwrap()));
    assert!(result.fields.is_empty());
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

## 6. Config Sources vs Built-in Providers

### When to use a config Source

A config Source is defined in `~/.config/beachcomber/config.toml` (or `conf.d/`) without writing any Rust. Use it when:

- The logic is simple or already exists as a shell script
- The tool does not have a file-based state representation (forced to shell out)
- The data changes infrequently so the performance cost is acceptable
- You need something working today and can write a built-in later

### How config Sources work

A Source entry in config:

```toml
[daemon.providers.my_vpn.sources.main]
data = "{{ exec('vpn-status --json') | fromjson }}"
result = "{{ data }}"
return = "{{ result | watch('/etc/vpn/state') | poll('10s') }}"
```

This registers a `ConfigSource` (see `src/provider/config_source.rs`) that:
1. Runs `sh -c "vpn-status --json"` on the daemon when `data` is evaluated
2. Parses stdout as JSON (`| fromjson`); `| fromini` reads `key=value` lines
3. Returns the fields `result` names, or the whole object for `result = "{{ data }}"`

The `return` line's decorators map onto `InvalidationStrategy` (see `src/shape/decorators.rs`):
- `poll` only -> `Poll { interval_secs, count }`
- `watch` only -> `Watch { patterns, abs_paths }`
- Both -> `WatchAndPoll { patterns, abs_paths, interval_secs, count }`
- A Source with no `return` takes the abstract Source's `poll('60s', 12) | failback(3, '1s')` from `defaults/00-comb.toml`

Set `root` to make the Source path-scoped (`data` runs with the root as its working directory):

```toml
[daemon.providers.project_version.sources.main]
root = "{{ lup('VERSION') | dirname }}"
data = "{{ exec('cat VERSION 2>/dev/null || echo unknown') | trim }}"
result.version = "{{ data }}"
return = "{{ result | watch('VERSION') }}"
```

### When to write a built-in

Prefer a built-in provider when:

- **Performance matters**: The provider will be queried frequently (prompt, tmux, status bar) and `spawn_blocking` a process every 5-30s adds up
- **File parsing is required**: The tool stores state in a structured file (INI, TOML, plain text) that you can parse directly without spawning the tool
- **Cross-platform behaviour**: Shell semantics differ between sh and cmd.exe; Rust handles this uniformly
- **The provider will be broadly useful**: If most beachcomber users would want it, it belongs in the binary

The perf breakeven point: if direct file reading brings execution from `>1ms` to `<100µs`, write a built-in. If the tool must be shelled out anyway and the data changes slowly, a config Source is fine.

### Migrating a config Source to built-in

1. Identify what the Source does — which file does it read, or which binary does it call?
2. Check `docs/performance.md` to see if the tool has already been handled as a file read
3. Write the built-in following §2 above, matching the field names your existing config consumers expect
4. Remove the Source from config and register the built-in in `registry.rs`
5. Run `cargo bench --bench providers` before and after to verify the improvement

---

## 8. Plugins

A plugin is a shared library (`.so` on Linux, `.dylib` on macOS). A Source's `data` line can call into one with the daemon's `plugin(name, symbol, args...)` function. This avoids the per-run process spawn of `exec()` while letting the logic be written in any language that can export C symbols. A plugin is a function, not a provider: the Source around it declares the fields, the schedule and the root, as for any other Source.

### Calling a plugin

```toml
[daemon.providers.hello.sources.main]
data = "{{ plugin('hello', 'greet', 'world') }}"
result.greeting = "{{ data }}"
```

`plugin(name, symbol, args...)` calls `symbol` in the library `name` with `args` and returns the library's JSON result, parsed (a map, list, string, number, bool or null). The call blocks the Source's run for as long as the library takes.

- A `name` without a `/` is looked up as `lib<name>.dylib` (macOS) or `lib<name>.so` (Linux) in each directory of `[daemon.plugin] path`, in order; the first directory holding the file wins. `plugin('hello', ...)` loads `libhello.so` or `libhello.dylib`. Not found, it is an error naming the file and the directories searched: `plugin(): libhello.so not found on [daemon.plugin] path (/home/me/.config/beachcomber/plugins)`.
- A `name` containing a `/` is the library path as given: absolute, or `~/...` (expanded from the daemon's `HOME`). A relative path is an error.
- A library is loaded on the first call that names it and stays loaded for the daemon's life. The daemon does not look at the file again: a name keeps the library it first loaded, even if another appears earlier on `path`; a name first used after a rebuild loads the rebuilt file; restart the daemon to move every name. Names that reach the same file (a symlink, a hard link) share one loaded library.

```toml
[daemon.plugin]
path = ["~/.local/lib/beachcomber", "/opt/beachcomber/plugins"]
```

`path` is a list of directories (absolute, or starting `~/`), default `["<config dir>/plugins"]` (`~/.config/beachcomber/plugins`). A list in a later file replaces the earlier one whole. A key other than `path`, or a `path` that is not a list of directory strings, is a config error. Changing `[daemon.plugin]` restarts the daemon.

### The C ABI

Every symbol is the same function type:

```c
const char* symbol(const char* input);
```

- `input` is the call's arguments as a NUL-terminated JSON array: `plugin('hello', 'greet', 'world', 3)` passes `["world",3]`.
- The return value is a NUL-terminated JSON value (UTF-8) **owned by the library**, valid until the next call into the same library. The daemon copies it before it makes another call, and serialises calls into one library, so a static or thread-local buffer is enough; there is no free function.
- `NULL` means failure. The Source's run fails with `plugin(): <symbol> in <name>: returned NULL`; a symbol that is missing, output that is not UTF-8 and output that is not JSON each have their own message.
- The function **must not unwind.** A panic or C++ exception escaping an `extern "C"` function aborts the daemon. In Rust, catch panics inside the function (`std::panic::catch_unwind`) and return `NULL`.
- The function may be called from any of the daemon's blocking threads (one at a time per library); do not rely on thread-local state surviving between calls.

### Example: a minimal C plugin

```c
#include <stdio.h>

// plugin('hello', 'greet', 'world')  ->  "hello, world"
static char out[256];

const char* greet(const char* input) {
    char name[128];
    // input is ["world"]; a real plugin parses it with a JSON library
    if (sscanf(input, "[\"%127[^\"]\"]", name) != 1) return NULL;
    snprintf(out, sizeof out, "\"hello, %s\"", name);
    return out;
}
```

Build: `cc -shared -fPIC -o libhello.so hello.c` (Linux) or `cc -shared -o libhello.dylib hello.c` (macOS), and put the file in `~/.config/beachcomber/plugins/`. With the Source above, `comb get hello.greeting` prints `hello, world`.

Plugins are for work where an `exec()` spawn is too expensive or the logic does not belong in the daemon binary: compiled parsing, a vendor SDK, a local socket protocol. A plugin runs inside the daemon with its permissions; only put libraries you trust on `[daemon.plugin] path`.
