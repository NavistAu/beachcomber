---
sidebar_position: 2
title: Provider Development
---

# Provider Development Guide

Step-by-step reference for writing new built-in providers. Built-in providers live in `src/provider/` and are compiled into the daemon. For a lower-effort path using shell scripts, see §6.

---

## 1. The Provider Trait

Every provider implements this trait (defined in `src/provider/mod.rs`):

```rust
pub trait Provider: Send + Sync {
    fn metadata(&self) -> ProviderMetadata;
    fn execute(&self, path: Option<&str>) -> Option<ProviderResult>;
}
```

**`metadata()`** is called at registration time and on every `comb l` request. It must be fast and allocation-light (it currently allocates; a future optimisation may switch to `Cow<'static, str>`). Return a `ProviderMetadata` describing:

- `name`: the provider's key used in `comb g <name>.<field>`
- `fields`: a list of `FieldSchema { name, field_type }` describing what fields `execute()` will populate
- `invalidation`: when the cached value should be refreshed (see §3)
- `global`: `true` if the provider ignores the `path` argument (e.g., `hostname`, `user`); `false` if it is path-scoped (e.g., `git`, `terraform`)

**`execute(path)`** runs the provider and returns the result. It is called on a blocking thread pool (`tokio::task::spawn_blocking`), so it may safely call `std::process::Command`, `std::fs::read_to_string`, and other blocking operations. Return `None` to indicate that no value is available (the cache will not be updated). Return `Some(ProviderResult)` on success.

`ProviderResult` is a `HashMap<String, Value>` wrapper. Insert fields with `result.insert("fieldname", Value::String("..."))`.

---

## 2. Step-by-Step: Writing a "docker context" Provider

This section builds a complete provider that reports the current Docker context name and endpoint.

Docker stores its active context in `~/.docker/config.json` (field `"currentContext"`) and context details in `~/.docker/contexts/meta/<hash>/meta.json`. Reading these files directly is ~1µs, versus ~30ms for `docker context inspect`.

### 2.1 Create the file

Create `src/provider/dockercontext.rs`:

```rust
use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::path::PathBuf;

pub struct DockerContextProvider;

impl Provider for DockerContextProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "dockercontext".to_string(),
            fields: vec![
                FieldSchema { name: "name".to_string(), field_type: FieldType::String },
                FieldSchema { name: "endpoint".to_string(), field_type: FieldType::String },
            ],
            invalidation: InvalidationStrategy::Watch {
                patterns: vec![
                    home_subpath(".docker/config.json"),
                    home_subpath(".docker/contexts"),
                ],
                fallback_poll_secs: Some(60),
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let home = std::env::var("HOME").ok()?;
        let config_path = PathBuf::from(&home).join(".docker").join("config.json");

        let config_text = std::fs::read_to_string(&config_path).ok()?;
        let config: serde_json::Value = serde_json::from_str(&config_text).ok()?;

        let context_name = config
            .get("currentContext")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();

        // Look up the endpoint from the context metadata.
        let endpoint = read_context_endpoint(&home, &context_name)
            .unwrap_or_else(|| "unix:///var/run/docker.sock".to_string());

        let mut result = ProviderResult::new();
        result.insert("name", Value::String(context_name));
        result.insert("endpoint", Value::String(endpoint));
        Some(result)
    }
}

fn home_subpath(rel: &str) -> String {
    std::env::var("HOME")
        .map(|h| format!("{}/{}", h, rel))
        .unwrap_or_else(|_| rel.to_string())
}

fn read_context_endpoint(home: &str, context_name: &str) -> Option<String> {
    if context_name == "default" {
        return None;
    }

    // Docker names contexts by SHA256 of the name; iterate the meta directory.
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

### 2.2 Register the provider

Add the module to `src/provider/mod.rs`:

```rust
pub mod dockercontext;
```

Add the import and registration to `src/provider/registry.rs`:

```rust
use crate::provider::dockercontext::DockerContextProvider;

// In with_defaults() and in the builtins vec inside with_config():
("dockercontext", Box::new(DockerContextProvider)),
```

### 2.3 Config (optional, for disabling)

No config entry is required. Users can disable it via `~/.config/beachcomber/config.toml`:

```toml
[providers.dockercontext]
enabled = false
```

### 2.4 Use it

```bash
comb g dockercontext.name
comb g dockercontext.endpoint
```

### 2.5 Write a test

Add a test module at the bottom of `src/provider/dockercontext.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_valid() {
        let provider = DockerContextProvider;
        let meta = provider.metadata();
        assert_eq!(meta.name, "dockercontext");
        assert!(meta.global);
        assert_eq!(meta.fields.len(), 2);
        assert!(meta.fields.iter().any(|f| f.name == "name"));
        assert!(meta.fields.iter().any(|f| f.name == "endpoint"));
    }

    #[test]
    fn returns_none_without_docker_config() {
        // Point HOME at a temp directory with no .docker/ directory.
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", dir.path());
        let provider = DockerContextProvider;
        let result = provider.execute(None);
        assert!(result.is_none());
        // Restore HOME to avoid contaminating other tests.
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
        let provider = DockerContextProvider;
        let result = provider.execute(None).unwrap();
        assert_eq!(
            result.get("name"),
            Some(&Value::String("default".to_string()))
        );
        std::env::remove_var("HOME");
    }
}
```

Run with:

```bash
cargo nextest run -p beachcomber -E 'test(provider::dockercontext)'
# or, with plain cargo test:
cargo test -p beachcomber provider::dockercontext
```

---

## 3. InvalidationStrategy: Choosing the Right Variant

```rust
pub enum InvalidationStrategy {
    Once,
    Poll { interval_secs: u64, floor_secs: u64 },
    Watch { patterns: Vec<String>, fallback_poll_secs: Option<u64> },
    WatchAndPoll { patterns: Vec<String>, interval_secs: u64, floor_secs: u64 },
}
```

**`Once`** — compute once at daemon startup, never again. Use for values that cannot change without a daemon restart: hostname, current user, static environment facts. Cost: one execution at startup, zero ongoing overhead.

```rust
// hostname: never changes while daemon is running
invalidation: InvalidationStrategy::Once,
```

**`Poll { interval_secs, floor_secs }`** — re-execute on a timer. Use when there is no file to watch that reliably reflects state changes. `floor_secs` prevents consumer-requested poll intervals from going below a minimum (usually 1). The interval is in seconds; `interval_secs: 30` means re-run every 30 seconds.

```rust
// battery level: no file to watch reliably, poll every 30s
invalidation: InvalidationStrategy::Poll {
    interval_secs: 30,
    floor_secs: 1,
},
```

**`Watch { patterns, fallback_poll_secs }`** — re-execute when the filesystem paths in `patterns` change. Use when there is a file or directory that is written whenever the state changes. `fallback_poll_secs` is used as a poll interval on systems where file watching fails or is unavailable. Set it to `Some(60)` unless freshness is critical.

```rust
// kubecontext: re-run when kubeconfig is written
invalidation: InvalidationStrategy::Watch {
    patterns: vec!["/home/user/.kube/config".to_string()],
    fallback_poll_secs: Some(60),
},
```

In practice, `patterns` should use absolute paths where possible. For paths relative to `$HOME`, expand them in `metadata()` using `std::env::var("HOME")` (see the `dockercontext` example above).

**`WatchAndPoll { patterns, interval_secs, floor_secs }`** — watch files AND poll on a timer. Use when file watching catches most changes quickly but some changes don't touch a watchable file (e.g., network-propagated git changes that arrive via `git fetch`). The `git` provider uses this: it watches `.git` for local operations and polls every 60 seconds to catch remote state.

```rust
// git: watch .git for local commits/checkouts, poll every 60s for remote changes
invalidation: InvalidationStrategy::WatchAndPoll {
    patterns: vec![".git".to_string()],
    interval_secs: 60,
    floor_secs: 1,
},
```

Note that for path-scoped providers (e.g., `git`), patterns like `".git"` are relative to the queried path and the FsWatcher receives the resolved absolute path when demand is first registered. For global providers, patterns should be absolute paths.

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

**Rule 4: Providers must be stateless.**

`execute()` receives no mutable state. Do not use `Mutex`-wrapped fields inside your provider struct to cache intermediate results — this adds contention and complexity. If two concurrent calls to `execute()` are needed (different paths), they must be independent.

See `docs/performance.md` for the full performance profile, benchmark commands, and the regression checklist.

---

## 5. Testing Patterns

**Basic structure**

Every provider file should have a `#[cfg(test)]` module. At minimum, test:
1. `metadata()` returns valid, expected values
2. `execute()` returns `None` when the required tool/file is absent
3. `execute()` returns the expected fields when given a valid fixture

**Using tempdir**

For providers that read files, use `tempfile::tempdir()` to create a controlled environment:

```rust
#[test]
fn detects_git_repo() {
    let dir = tempfile::tempdir().unwrap();
    // Create a minimal .git directory
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    let provider = GitProvider;
    // execute() returns None for a bare .git dir without a valid git repo state,
    // but it should not panic.
    let _ = provider.execute(Some(dir.path().to_str().unwrap()));
}
```

**Testing with real git repos**

For providers that shell out (like `git`), test against a real initialized repo:

```rust
#[test]
fn git_status_on_empty_repo() {
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
    let result = provider.execute(Some(dir.path().to_str().unwrap()));
    assert!(result.is_some());
    let result = result.unwrap();
    assert_eq!(result.get("branch"), Some(&Value::String("main".to_string())));
    assert_eq!(result.get("dirty"), Some(&Value::Bool(false)));
}
```

**Testing when the external tool is not installed**

Providers that depend on optional tools (`docker`, `kubectl`, `aws`) must return `None` gracefully when the tool is absent or when the relevant config files do not exist. Test this by pointing `HOME` to a clean tempdir:

```rust
#[test]
fn returns_none_without_kubeconfig() {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", dir.path());
    std::env::remove_var("KUBECONFIG");

    let provider = KubecontextProvider;
    assert!(provider.execute(None).is_none());

    std::env::remove_var("HOME");
}
```

Avoid `std::env::set_var` in parallel tests — it mutates global state. Either mark such tests `#[serial]` (via the `serial_test` crate), run them with `cargo nextest run --test-threads=1 <filter>`, or use `cargo test -- --test-threads=1`. Note: cargo-nextest runs each test in its own process by default, so env-var mutations are already isolated per test — the `--test-threads=1` workaround is mainly relevant for plain `cargo test`.

**Testing `metadata()` completeness**

A quick structural test catches registration bugs early:

```rust
#[test]
fn metadata_fields_match_execute_output() {
    let dir = tempfile::tempdir().unwrap();
    // ... set up fixture ...
    let provider = DockerContextProvider;
    let meta = provider.metadata();
    let result = provider.execute(None).unwrap();

    for field in &meta.fields {
        assert!(
            result.get(&field.name).is_some(),
            "metadata declares field '{}' but execute() did not populate it",
            field.name
        );
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
3. Returns the parsed fields as a `ProviderResult`

The `invalidation` config maps directly to `InvalidationStrategy`:
- `poll` only -> `Poll { interval_secs }`
- `watch` only -> `Watch { patterns, fallback_poll_secs: Some(60) }`
- Both -> `WatchAndPoll { patterns, interval_secs }`
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

## 7. Shared Library Providers

Shared library providers load a `.so` (Linux) or `.dylib` (macOS) at daemon startup and call into it via a C ABI. This avoids the per-execution process spawn cost of script providers while allowing provider logic to be written in any language that can produce a shared library with C-compatible exports.

### C ABI Contract

Libraries must export three symbols:

```c
// Return JSON metadata: {"fields":{"name":"string",...}, "invalidation":{"poll":"30s"}, "global":true}
// Caller frees the returned pointer via beachcomber_provider_free().
const char* beachcomber_provider_metadata(void);

// Execute the provider. path is NULL for global providers.
// Return a JSON object of field values, or NULL on failure.
// Caller frees the returned pointer via beachcomber_provider_free().
const char* beachcomber_provider_execute(const char* path);

// Free a string previously returned by metadata or execute.
void beachcomber_provider_free(char* ptr);
```

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

If the library's `beachcomber_provider_metadata()` returns valid JSON, that metadata is used. Otherwise, metadata falls back to the TOML config fields (same as script providers).

### When to Use

- **Performance-critical providers** where even a 2-6ms process spawn is too expensive
- **Complex logic** that benefits from compiled code but doesn't belong in the daemon binary
- **Third-party plugins** distributed as shared libraries
- **Language flexibility** — write providers in C, C++, Rust, Go (with cgo), Zig, or any language that can export C symbols

### Example: Minimal C Provider

```c
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

const char* beachcomber_provider_metadata(void) {
    const char* json = "{\"fields\":{\"value\":\"string\"},\"invalidation\":{\"poll\":\"10s\"},\"global\":true}";
    char* result = strdup(json);
    return result;
}

const char* beachcomber_provider_execute(const char* path) {
    (void)path;
    char* result = strdup("{\"value\":\"hello from C\"}");
    return result;
}

void beachcomber_provider_free(char* ptr) {
    free(ptr);
}
```

Build: `cc -shared -o libhello.so hello_provider.c` (Linux) or `cc -shared -o libhello.dylib hello_provider.c` (macOS).

Configure:
```toml
[providers.hello]
type = "library"
library_path = "/path/to/libhello.so"
```

Test: `comb g hello.value` → `hello from C`
