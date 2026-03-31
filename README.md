# beachcomber

> One daemon. One cache. Every consumer reads from it.

---

## The Problem

Picture your typical development machine. You have 30 terminal shells open — tmux, iTerm tabs, nested sessions. Each one is running powerlevel10k with gitstatus enabled.

**That's 30 gitstatusd daemons.** Each one spawns a thread pool of up to `min(32, 2 × NUM_CPUS)` threads. On a 16-core machine, that's 960 threads — all independently watching overlapping filesystem trees, all independently computing the same answer to "what branch am I on?"

Now look at your tmux status bar. If you're using a common config like gpakosz/.tmux, it forks a shell process for every pane to collect battery, hostname, and git data. Every 10 seconds. 50 panes × 10 data points = **500 shell forks every 10 seconds.** Your laptop is burning CPU to spawn processes that each run for 5ms and return the same answer they returned 10 seconds ago.

Meanwhile, `fseventsd` is pegging a CPU core dispatching the same filesystem change event to 30 independent FSEvents registrations — one per gitstatusd instance — all watching the same `.git` directory.

Every shell, every editor plugin, every status bar, every prompt framework is independently asking the same questions about the same files with no coordination whatsoever.

**beachcomber** is a single daemon that watches directories, computes shell state, and caches it. Every consumer — prompts, tmux, editors, scripts — reads from the same cache via a Unix socket. One watcher. One computation. Infinite readers.

---

## The Numbers

| Operation | Latency |
|---|---|
| Cache read (global key) | **157 ns** |
| Socket query (warm, persistent connection) | **15 µs** |
| Socket query (cold, new connection) | 34 µs |
| Git status (at parity with raw `git status`) | **5.6 ms** |
| Throughput (10 concurrent clients) | **45,000 req/sec** |

**Real-world impact:**

| Scenario | Without beachcomber | With beachcomber | Improvement |
|---|---|---|---|
| zsh prompt (3 queries) | ~5ms (gitstatus fork) | **45µs** (persistent session) | 111x faster |
| tmux status (100 panes, 10s refresh) | ~2.5s CPU (500 shell forks) | **7.5ms** (socket queries) | 333x fewer forks |
| fseventsd dispatch load | N watchers × N events | 1 watcher, shared | Linear reduction |

---

## Quick Look

```
$ comb get git .
{
  "ok": true,
  "data": {
    "branch": "main",
    "commit": "a1b2c3d",
    "detached": false,
    "upstream": "origin/main",
    "tag": "v0.3.1",
    "dirty": true,
    "staged": 2,
    "unstaged": 1,
    "untracked": 4,
    "conflicted": 0,
    "ahead": 0,
    "behind": 0,
    "stash": 1,
    "lines_added": 47,
    "lines_removed": 12,
    "lines_staged_added": 23,
    "lines_staged_removed": 5,
    "state": "clean",
    "state_step": 0,
    "state_total": 0,
    "last_commit_age_secs": 3420
  },
  "age_ms": 120,
  "stale": false
}

$ comb get git.branch . -f text
main

$ comb get hostname.short -f text
Project2501

$ comb get battery
{
  "ok": true,
  "data": {
    "percent": 85,
    "charging": true,
    "time_remaining": 0
  },
  "age_ms": 8400,
  "stale": false
}

$ comb status
{
  "uptime_secs": 3642,
  "cache_entries": 12,
  "active_watchers": 3,
  "demand": 8
}
```

---

## Table of Contents

- [Quick Start](#quick-start)
- [How It Works](#how-it-works)
- [Consumer Integration](#consumer-integration)
- [Shell Fallback Function](#shell-fallback-function)
- [Configuration Reference](#configuration-reference)
- [Built-in Providers Reference](#built-in-providers-reference)
- [Custom Providers Guide](#custom-providers-guide)
- [CLI Reference](#cli-reference)
- [Debugging](#debugging)
- [Protocol Reference](#protocol-reference)
- [Alternatives and Prior Art](#alternatives-and-prior-art)
- [FAQ](#faq)
- [Contributing](#contributing)

---

## Quick Start

### Install

```sh
# Homebrew (macOS)
brew install beachcomber

# Cargo (Rust toolchain required)
cargo install beachcomber
```

### Verify

The daemon starts automatically on first use — no setup required.

```sh
# Query your current git branch (run from inside a git repo)
comb get git.branch . -f text

# Query battery
comb get battery.percent -f text

# Check daemon status
comb status
```

That's it. The daemon started in the background when you ran that first query.

### Try it in your prompt

```sh
# Add to ~/.zshrc
precmd() {
    PS1="%F{blue}$(comb get git.branch . -f text 2>/dev/null)%f %# "
}
```

Source your `.zshrc` and open a few more shells. Then run `comb status` — you'll see the cache entry being shared across all shells, with a single filesystem watcher covering all of them.

---

## How It Works

beachcomber is a single async daemon that:

1. Serves queries from consumers (prompts, status bars, editors) via a Unix socket
2. Watches filesystem directories using native FSEvents (macOS) or inotify (Linux)
3. Executes providers when files change or poll timers fire — not on every query
4. Caches results in a shared in-memory map (157ns reads)
5. Returns cached data instantly to any number of concurrent readers

The daemon is socket-activated: it starts automatically when any client connects, and shuts down after an idle period when all connections drop.

```
                    ┌─────────────────────────────────────┐
                    │          beachcomber daemon           │
                    │                                       │
  filesystem ──────►│  FSEvents/inotify                     │
  changes           │       │                               │
                    │       ▼                               │
                    │  Scheduler ──► Providers ──► Cache   │
                    │                  git          157ns   │
                    │                  battery      reads   │
                    │                  network              │
                    │                  hostname             │
                    │                  ...                  │
                    │                  scripts              │
                    │                  (your own)           │
                    │                                       │
                    │  Unix Socket Server                   │
                    └──────────────┬──────────────────────-┘
                                   │
                    ┌──────────────┼────────────────────┐
                    │              │                     │
               zsh prompt     tmux status           neovim
               bash prompt    polybar/waybar         lualine
               fish prompt    sketchybar             scripts
               starship       oh-my-posh             CI/automation
```

**Providers are never re-executed on every query.** A git status is computed once when `.git` changes, then served from cache to every reader — whether that's one prompt or a hundred tmux panes. The filesystem watcher is registered once for all concurrent readers.

**Connection context** means consumers can set a working directory once on connect. `comb get git.branch` without an explicit path uses the connection's context directory, making prompt integration natural.

**Demand-driven lifecycle:** the daemon watches nothing until queried. Each `get` request signals demand, keeping the provider warm automatically. Resource usage scales with actual query patterns. Entries enter a backoff/drain sequence after queries stop — staying warm for a grace period (30s default) in case a new shell opens, then progressively slowing and eventually evicting.

---

## Consumer Integration

### zsh prompt (precmd hook)

The most common use case. Use `precmd` to refresh prompt variables before each prompt draw. A persistent `ClientSession` amortizes the connection cost across multiple queries — three fields for the price of one connection.

```zsh
# ~/.zshrc
precmd() {
    local branch dirty untracked
    branch=$(comb get git.branch . -f text 2>/dev/null)
    dirty=$(comb get git.dirty . -f text 2>/dev/null)
    untracked=$(comb get git.untracked . -f text 2>/dev/null)

    local git_part=""
    if [[ -n "$branch" ]]; then
        git_part="%F{blue}${branch}%f"
        [[ "$dirty" == "true" ]] && git_part+="*"
        [[ "$untracked" -gt 0 ]] && git_part+="?"
        git_part+=" "
    fi

    PS1="${git_part}%F{green}%~%f %# "
}
```

### tmux status bar (format string replacement)

tmux evaluates `#(command)` format strings to populate the status bar. Each `#()` is a subprocess — beachcomber makes these essentially free because the daemon is already running.

```tmux
# ~/.tmux.conf

# Battery percentage and git branch in right status
set -g status-right '#(comb get battery.percent -f text)%% bat | #(comb get git.branch . -f text)'

# Left: session name + kubernetes context
set -g status-left '[#S] #(comb get kubecontext.context -f text)'

# Refresh interval — lower is fine because queries cost almost nothing
set -g status-interval 5
```

**Why this is different from the problem described above:** each `#()` invocation still forks a shell, but `comb` reads a pre-cached value in ~34µs instead of spawning git (5ms+) or running a battery subprocess (6ms). The total time savings across a 50-pane tmux session is substantial.

The simple `#()` approach shown above is already a major improvement over shelling out to git or battery commands directly. Each `comb get` also signals demand to the daemon, keeping the provider warm automatically.

### bash prompt (PROMPT_COMMAND)

bash runs `PROMPT_COMMAND` before each prompt. Parse the `key=value` text output from a whole-provider query to minimize subprocess calls.

```bash
# ~/.bashrc
__beachcomber_prompt() {
    # Fetch entire git state in one query, parse key=value output
    local git_state
    git_state=$(comb get git . -f text 2>/dev/null)

    local branch dirty
    while IFS='=' read -r key value; do
        case "$key" in
            branch) branch="$value" ;;
            dirty)  dirty="$value" ;;
        esac
    done <<< "$git_state"

    local git_part=""
    [[ -n "$branch" ]] && git_part="(${branch}${dirty:+*}) "

    local kube
    kube=$(comb get kubecontext.context -f text 2>/dev/null)
    local kube_part=""
    [[ -n "$kube" ]] && kube_part="[${kube}] "

    PS1="${kube_part}${git_part}\w \$ "
}

PROMPT_COMMAND=__beachcomber_prompt
```

### fish prompt function

fish's `fish_prompt` function is called before each prompt. fish has no subshell penalty for command substitutions, so this is already efficient.

```fish
# ~/.config/fish/functions/fish_prompt.fish
function fish_prompt
    set -l branch (comb get git.branch . -f text 2>/dev/null)
    set -l dirty (comb get git.dirty . -f text 2>/dev/null)
    set -l battery (comb get battery.percent -f text 2>/dev/null)

    set -l git_info ""
    if test -n "$branch"
        set git_info " $branch"
        test "$dirty" = "true"; and set git_info "$git_info*"
    end

    set -l bat_info ""
    if test -n "$battery"
        set bat_info " $battery%%"
    end

    echo -n (set_color blue)(prompt_pwd)(set_color normal)$git_info$bat_info" > "
end
```

### neovim statusline (Lua via vim.loop)

neovim plugins can connect directly to the beachcomber socket using `vim.loop` (libuv). This gives the editor real-time git state without spawning `git` on every buffer switch or save.

```lua
-- In your statusline plugin or init.lua
local function get_beachcomber(key, path, callback)
    local runtime_dir = os.getenv("XDG_RUNTIME_DIR")
    local tmpdir = os.getenv("TMPDIR") or "/tmp"
    local uid = vim.loop.getuid()

    local sock_path
    if runtime_dir then
        sock_path = runtime_dir .. "/beachcomber/sock"
    else
        sock_path = tmpdir .. "/beachcomber-" .. uid .. "/sock"
    end

    local sock = vim.loop.new_pipe()
    sock:connect(sock_path, function(err)
        if err then callback(nil); return end

        local request = vim.json.encode({
            op = "get",
            key = key,
            path = path,
            format = "text"
        }) .. "\n"

        sock:write(request)
        sock:read_start(function(err2, data)
            sock:close()
            if err2 or not data then callback(nil); return end
            local ok, response = pcall(vim.json.decode, data)
            if ok and response.ok and response.data then
                callback(response.data)
            else
                callback(nil)
            end
        end)
    end)
end

-- Usage in a statusline
local function git_branch()
    local cwd = vim.fn.getcwd()
    local result = ""
    get_beachcomber("git.branch", cwd, function(branch)
        if branch then result = " " .. branch end
    end)
    return result
end
```

For a synchronous alternative, `comb get git.branch . -f text` in a `jobstart` or `system()` call works fine — the 34µs round-trip is negligible compared to UI update frequency.

### starship custom module

starship's `[custom.*]` modules run a shell command and display its output. Using beachcomber as the backend replaces starship's per-prompt git computation with a cache read.

```toml
# ~/.config/starship.toml

# Replace starship's built-in git_branch with a beachcomber-backed one
[git_branch]
disabled = true

[custom.git_branch]
command = "comb get git.branch . -f text"
when = "comb get git.branch . -f text"
format = "[$output]($style) "
style = "bold blue"
description = "Git branch via beachcomber"

[custom.git_dirty]
command = 'test "$(comb get git.dirty . -f text)" = "true" && echo "*"'
when = "comb get git.dirty . -f text"
format = "[$output]($style)"
style = "bold red"

[custom.kube]
command = "comb get kubecontext.context -f text"
when = "comb get kubecontext.context -f text"
format = "[$output]($style) "
style = "bold cyan"
symbol = "☸ "
```

### polybar / waybar / sketchybar custom module

Status bars on Linux (polybar, waybar) and macOS (sketchybar) poll external commands for dynamic content. beachcomber makes the polling interval irrelevant — each query costs microseconds.

**polybar:**
```ini
[module/git]
type = custom/script
exec = comb get git.branch . -f text
interval = 5
format = <label>
label = %output%

[module/battery]
type = custom/script
exec = comb get battery.percent -f text
interval = 30
format = <label>
label = BAT: %output%%%

[module/network]
type = custom/script
exec = comb get network.ssid -f text
interval = 10
```

**waybar (JSON module):**
```json
"custom/git": {
    "exec": "comb get git.branch . -f text",
    "interval": 5,
    "format": " {}",
    "tooltip": false
},
"custom/battery": {
    "exec": "comb get battery.percent -f text",
    "interval": 30,
    "format": " {}%"
}
```

**sketchybar:**
```sh
# In your sketchybarrc
sketchybar --add item git_branch right \
           --set git_branch update_freq=5 \
                            script="sketchybar --set git_branch label=\"$(comb get git.branch . -f text)\""
```

### Python script

Connect directly to the Unix socket from any language. beachcomber's protocol is newline-delimited JSON — no dependencies required.

```python
import socket
import json
import os

def beachcomber_get(key, path=None):
    runtime_dir = os.environ.get("XDG_RUNTIME_DIR")
    if runtime_dir:
        sock_path = os.path.join(runtime_dir, "beachcomber", "sock")
    else:
        import ctypes
        uid = os.getuid()
        tmpdir = os.environ.get("TMPDIR", "/tmp")
        sock_path = os.path.join(tmpdir, f"beachcomber-{uid}", "sock")

    request = {"op": "get", "key": key}
    if path:
        request["path"] = path

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(sock_path)
    sock.send((json.dumps(request) + "\n").encode())

    # Read until newline
    data = b""
    while not data.endswith(b"\n"):
        data += sock.recv(4096)
    sock.close()

    response = json.loads(data.decode())
    if response.get("ok") and response.get("data") is not None:
        return response["data"]
    return None

# Examples
branch = beachcomber_get("git.branch", "/path/to/repo")
battery = beachcomber_get("battery.percent")
git_state = beachcomber_get("git", "/path/to/repo")  # returns full dict
print(f"Branch: {branch}, Battery: {battery}%")
```

### Shell one-liner for scripts and CI

For scripts that want to annotate output with git context but don't require beachcomber to be installed:

```sh
# Returns branch name — uses beachcomber if available, falls back to git
BRANCH=$(comb get git.branch . -f text 2>/dev/null || git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "unknown")

# In CI, log the current branch alongside build output
echo "Building branch: $(comb get git.branch . -f text 2>/dev/null || git rev-parse --abbrev-ref HEAD)"

# Check if repo is dirty before deploying
if [ "$(comb get git.dirty . -f text 2>/dev/null)" = "true" ]; then
    echo "Warning: uncommitted changes"
fi
```

### Rust SDK (`beachcomber-client`)

For Rust consumers, the `beachcomber-client` crate provides a typed, synchronous API with no tokio dependency:

```toml
[dependencies]
beachcomber-client = "0.1"
```

```rust
use beachcomber_client::{Client, CombResult};

let client = Client::new(); // auto-discovers socket, starts daemon if needed

// Single field query
match client.get("git.branch", Some("/path/to/repo"))? {
    CombResult::Hit { data, .. } => println!("branch: {}", data.as_text().unwrap()),
    CombResult::Miss => println!("not cached yet — will be ready on next query"),
}

// Full provider query with typed field access
match client.get("git", Some("/path/to/repo"))? {
    CombResult::Hit { data, age_ms, stale } => {
        println!("branch: {}", data.get_str("branch").unwrap_or("?"));
        println!("dirty: {}", data.get_bool("dirty").unwrap_or(false));
        println!("ahead: {}", data.get_i64("ahead").unwrap_or(0));
        println!("age: {}ms, stale: {}", age_ms, stale);
    }
    CombResult::Miss => {}
}

// Persistent session for multiple queries (one connection, multiple requests)
let mut session = client.session()?;
session.set_context("/path/to/repo")?;
let branch = session.get("git.branch", None)?;
let battery = session.get("battery.percent", None)?;
```

Features:
- **Synchronous** — no async runtime needed
- **Socket activation** — starts the daemon automatically if not running
- **Typed access** — `get_str()`, `get_bool()`, `get_i64()`, `get_f64()`
- **Persistent sessions** — reuse one connection for multiple queries (15µs/query vs 34µs)
- **Configurable timeouts** — default 100ms, adjustable via `ClientConfig`

---

## Shell Fallback Function

Apps that want to support beachcomber without requiring it can embed a fallback function. If `comb` is not installed, the function falls back to the native tool. This pattern lets any shell script or prompt framework opt into beachcomber acceleration transparently.

**bash / zsh:**
```sh
# Returns the current git branch name.
# Uses beachcomber if installed; falls back to git directly.
_git_branch() {
    if command -v comb >/dev/null 2>&1; then
        comb get git.branch . -f text 2>/dev/null && return
    fi
    git rev-parse --abbrev-ref HEAD 2>/dev/null
}

# Returns "true" if the working tree has uncommitted changes.
_git_dirty() {
    if command -v comb >/dev/null 2>&1; then
        comb get git.dirty . -f text 2>/dev/null && return
    fi
    git diff --quiet 2>/dev/null || echo "true"
}

# Returns current kubernetes context.
_kube_context() {
    if command -v comb >/dev/null 2>&1; then
        comb get kubecontext.context -f text 2>/dev/null && return
    fi
    kubectl config current-context 2>/dev/null
}

# Returns battery percentage as an integer.
_battery_percent() {
    if command -v comb >/dev/null 2>&1; then
        comb get battery.percent -f text 2>/dev/null && return
    fi
    # macOS fallback
    pmset -g batt 2>/dev/null | grep -Eo '[0-9]+%' | head -1 | tr -d '%'
}
```

**fish:**
```fish
function _git_branch
    if command -q comb
        comb get git.branch . -f text 2>/dev/null; and return
    end
    git rev-parse --abbrev-ref HEAD 2>/dev/null
end

function _git_dirty
    if command -q comb
        comb get git.dirty . -f text 2>/dev/null; and return
    end
    git diff --quiet 2>/dev/null; or echo "true"
end

function _kube_context
    if command -q comb
        comb get kubecontext.context -f text 2>/dev/null; and return
    end
    kubectl config current-context 2>/dev/null
end

function _battery_percent
    if command -q comb
        comb get battery.percent -f text 2>/dev/null; and return
    end
    # macOS fallback
    pmset -g batt 2>/dev/null | string match -r '\d+%' | string replace '%' ''
end
```

These functions can be pasted directly into prompt frameworks, dotfile repos, or shared shell libraries. Users with beachcomber installed get the 15µs path; users without it get the native fallback. No beachcomber dependency required.

### Inline Fallback with `||`

For scripts that only need a value once (not in a hot loop like a prompt), the wrapper function is unnecessary. `comb` exits non-zero when it's not installed, not running, or the key doesn't exist — so a simple `||` chain works:

```sh
# Single assignment, no wrapper needed
branch=$(comb get git.branch . -f text 2>/dev/null || git rev-parse --abbrev-ref HEAD 2>/dev/null)
dirty=$(comb get git.dirty . -f text 2>/dev/null || git diff --quiet 2>/dev/null || echo "true")

# Use the values
if [ -n "$branch" ]; then
    echo "on $branch"
fi
```

This keeps scripts portable with zero comb dependency — `2>/dev/null` swallows errors, the `||` falls through to the native tool, and there's nothing to source or import. Prefer this for standalone scripts; use the wrapper functions above for shared shell libraries where the pattern repeats.

---

## Configuration Reference

beachcomber runs with sensible defaults and requires no configuration. The optional config file lives at `~/.config/beachcomber/config.toml`.

```toml
# ~/.config/beachcomber/config.toml

# ─── Daemon ────────────────────────────────────────────────────────────────────

[daemon]

# Override the Unix socket path.
# Default: $XDG_RUNTIME_DIR/beachcomber/sock
#          Falls back to: $TMPDIR/beachcomber-<uid>/sock
socket_path = ""

# Log level for daemon output.
# Options: "error", "warn", "info", "debug", "trace"
# Default: "info"
# Logs go to: $XDG_STATE_HOME/beachcomber/daemon.log
log_level = "info"

# Maximum time (in seconds) to wait for any provider to complete.
# Providers that exceed this are cancelled; the last good cached value is retained.
# Default: 10
provider_timeout_secs = 10

# Path to an environment file loaded at daemon startup.
# Each line is KEY=VALUE (or KEY="VALUE"). Blank lines and #comments are ignored.
# These vars are available to ${VAR} expansion in HTTP headers, script commands, etc.
# Default: ~/.config/beachcomber/env (loaded automatically if present)
# env_file = "~/.config/beachcomber/env"


# ─── Lifecycle ─────────────────────────────────────────────────────────────────

[lifecycle]

# How long (in seconds) to maintain full polling cadence after demand expires
# (i.e., after the last query for a key). During this window, cache entries
# stay warm in case a new shell opens.
# Default: 30
grace_period_secs = 30

# How long (in seconds) after demand expires before a cache entry is fully
# evicted. The daemon enters a progressive backoff between grace expiry and
# eviction (2x poll at 30s, 4x poll at 2min, frozen at 5min).
# Default: 900 (15 minutes)
eviction_timeout_secs = 900

# How long (in seconds) the daemon waits with no active connections before
# shutting itself down. The next client connection will socket-activate a
# fresh instance.
# Set to null to disable idle shutdown (daemon stays resident permanently).
# Default: 300 (5 minutes)
idle_shutdown_secs = 300


# ─── Built-in Provider Overrides ───────────────────────────────────────────────
# Use [providers.<name>] to override defaults for any built-in provider.
# Only specify the fields you want to change.

# Disable a provider entirely (it will never execute or appear in results)
[providers.conda]
enabled = false

# Override polling interval and floor for battery
[providers.battery]
poll_secs = 60          # default: 30
poll_floor_secs = 10    # default: 5

# Make git refresh more frequently (useful on fast machines or large repos)
[providers.git]
poll_secs = 30          # default: no poll (filesystem-triggered only)
poll_floor_secs = 2     # default: not set

# Override network polling interval
[providers.network]
poll_secs = 30          # default: 10
poll_floor_secs = 10    # default: 5


# ─── Custom Script Providers ───────────────────────────────────────────────────
# Define your own providers backed by any executable.

# Minimal: a global provider that polls every 30 seconds
[providers.docker_context]
command = "docker context show"
output = "text"         # single-line output becomes { "value": "<output>" }
# or use output = "json" for structured output: { "key": value, ... }
# or use output = "kv" for key=value line format

[providers.docker_context.invalidation]
poll = "30s"

# A path-scoped provider that watches a file and has a poll fallback
[providers.node_version]
command = "node --version"
output = "text"
scope = "path"          # scoped to a directory; path argument required

[providers.node_version.invalidation]
watch = [".node-version", ".nvmrc", "package.json"]
poll = "60s"            # safety-net poll in case filesystem events are missed

# A provider with structured JSON output
[providers.cargo_meta]
command = "cargo metadata --format-version=1 --no-deps --quiet"
output = "json"         # parse stdout as JSON object; top-level keys become fields
scope = "path"

[providers.cargo_meta.invalidation]
watch = ["Cargo.toml", "Cargo.lock"]
poll = "120s"

# Explicitly disable a custom provider without removing its config
[providers.my_slow_thing]
command = "my-slow-script"
enabled = false


# ─── HTTP Providers ──────────────────────────────────────────────────────────
# Fetch data directly from REST APIs — no curl fork, no shell spawning.
# Uses in-process HTTP client with connection reuse.

# Basic: poll a status API
[providers.service_status]
type = "http"
url = "https://status.anthropic.com/api/v2/summary.json"
extract = "status"              # dot-path into the JSON response
                                # e.g., response.status.indicator → provider field "indicator"

[providers.service_status.invalidation]
poll = "60s"

# With auth headers (env vars expanded at runtime)
[providers.github_rate]
type = "http"
url = "https://api.github.com/rate_limit"
headers = { Authorization = "Bearer ${GITHUB_TOKEN}" }
extract = "rate"                # extracts { "limit": 5000, "remaining": 4999, ... }

[providers.github_rate.invalidation]
poll = "30s"

# Infrequent poll (daily)
[providers.exchange_rate]
type = "http"
url = "https://api.exchangerate-api.com/v4/latest/USD"
extract = "rates.AUD"           # extracts a single nested value

[providers.exchange_rate.invalidation]
poll = "86400s"
```

### Config field summary

**`[daemon]` section:**

| Field | Type | Default | Description |
|---|---|---|---|
| `socket_path` | string | `$XDG_RUNTIME_DIR/beachcomber/sock` | Unix socket path |
| `log_level` | string | `"info"` | Tracing log level |
| `provider_timeout_secs` | int | `10` | Max seconds for any provider to run |
| `env_file` | string | `~/.config/beachcomber/env` | Path to env file loaded at startup |

**`[lifecycle]` section:**

| Field | Type | Default | Description |
|---|---|---|---|
| `grace_period_secs` | int | `30` | Seconds at full cadence after demand expires |
| `eviction_timeout_secs` | int | `900` | Seconds until cache entry is fully evicted |
| `idle_shutdown_secs` | int or null | `null` (disabled) | Seconds until idle daemon shuts down |

**`[providers.<name>]` section (built-in overrides):**

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Set `false` to disable provider entirely |
| `poll_secs` | int | provider-specific | Override poll interval |
| `poll_floor_secs` | int | provider-specific | Minimum poll interval consumers can request |

**`[providers.<name>]` section (custom script providers):**

| Field | Type | Required | Description |
|---|---|---|---|
| `command` | string | yes | Shell command to execute |
| `output` | string | no | `"json"` (default), `"kv"`, or `"text"` |
| `scope` | string | no | `"global"` (default) or `"path"` |
| `enabled` | bool | no | `false` to disable |
| `poll_secs` | int | no | Poll interval in seconds |
| `poll_floor_secs` | int | no | Minimum poll interval |
| `invalidation.poll` | string | no | Poll interval as duration string (`"30s"`, `"2m"`) |
| `invalidation.watch` | array of strings | no | File/directory patterns to watch |

**`[providers.<name>]` section (HTTP providers):**

| Field | Type | Required | Description |
|---|---|---|---|
| `type` | string | yes | Must be `"http"` |
| `url` | string | yes | URL to fetch. Supports `${ENV_VAR}` expansion. |
| `method` | string | no | HTTP method: `"GET"` (default), `"POST"`, `"PUT"` |
| `headers` | table | no | HTTP headers. Values support `${ENV_VAR}` expansion. |
| `body` | string | no | Request body (for POST/PUT) |
| `extract` | string | no | Dot-separated path into the JSON response (e.g., `"status.indicator"`, `"rates.AUD"`) |
| `enabled` | bool | no | `false` to disable |
| `invalidation.poll` | string | no | Poll interval (default `"60s"`, floor `5s`) |

---

## Built-in Providers Reference

beachcomber ships 16 built-in providers organized by category.

### System

| Provider | Scope | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `hostname` | global | `name` (string), `short` (string) | once at startup | 400 ns |
| `user` | global | `name` (string), `uid` (int) | once at startup | 395 ns |
| `load` | global | `one` (float), `five` (float), `fifteen` (float) | poll 10s / floor 5s | 550 ns |
| `uptime` | global | `seconds` (int), `days` (int), `hours` (int), `minutes` (int) | poll 60s | 660 ns |
| `battery` | global | `percent` (int), `charging` (bool), `time_remaining` (int, seconds) | poll 30s / floor 5s | 6 ms |
| `network` | global | `interface` (string), `ip` (string), `vpn_active` (bool), `vpn_name` (string), `ssid` (string), `online` (bool) | poll 10s / floor 5s | 2 ms |

**Example output:**

```json
// comb get battery
{
  "ok": true,
  "data": { "percent": 78, "charging": false, "time_remaining": 7200 },
  "age_ms": 4200
}

// comb get network
{
  "ok": true,
  "data": {
    "interface": "en0",
    "ip": "192.168.1.42",
    "vpn_active": true,
    "vpn_name": "utun2",
    "ssid": "OfficeNet",
    "online": true
  },
  "age_ms": 3100
}

// comb get load
{
  "ok": true,
  "data": { "one": 2.34, "five": 1.87, "fifteen": 1.42 },
  "age_ms": 8900
}
```

### Git

| Provider | Scope | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `git` | path | 21 fields (see table below) | watch `.git` + fallback poll | 5.6 ms |

**Fields:**

| Field | Type | Description |
|---|---|---|
| `branch` | string | Current branch name |
| `commit` | string | Short SHA of HEAD |
| `detached` | bool | Whether HEAD is detached |
| `upstream` | string | Upstream tracking branch (e.g., "origin/main") |
| `tag` | string | Nearest tag (empty if none) |
| `dirty` | bool | Whether working tree has changes |
| `staged` | int | Number of staged files |
| `unstaged` | int | Number of unstaged modified files |
| `untracked` | int | Number of untracked files |
| `conflicted` | int | Number of conflicted files |
| `ahead` | int | Commits ahead of upstream |
| `behind` | int | Commits behind upstream |
| `stash` | int | Number of stash entries |
| `lines_added` | int | Lines added in working tree (unstaged) |
| `lines_removed` | int | Lines removed in working tree (unstaged) |
| `lines_staged_added` | int | Lines added in index (staged) |
| `lines_staged_removed` | int | Lines removed in index (staged) |
| `state` | string | Repo state: "clean", "merge", "rebase", "cherry-pick", "bisect", "revert" |
| `state_step` | int | Current step in rebase/cherry-pick (0 if not in progress) |
| `state_total` | int | Total steps in rebase/cherry-pick (0 if not in progress) |
| `last_commit_age_secs` | int | Seconds since last commit |

**Example output:**

```json
// comb get git .
{
  "ok": true,
  "data": {
    "branch": "feature/fast-cache",
    "commit": "a1b2c3d",
    "detached": false,
    "upstream": "origin/main",
    "tag": "v0.3.1",
    "dirty": true,
    "staged": 3,
    "unstaged": 1,
    "untracked": 0,
    "conflicted": 0,
    "ahead": 2,
    "behind": 0,
    "stash": 1,
    "lines_added": 47,
    "lines_removed": 12,
    "lines_staged_added": 23,
    "lines_staged_removed": 5,
    "state": "clean",
    "state_step": 0,
    "state_total": 0,
    "last_commit_age_secs": 3420
  },
  "age_ms": 234
}

// comb get git.branch . -f text
feature/fast-cache
```

### Cloud and DevOps

| Provider | Scope | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `kubecontext` | global | `context` (string), `namespace` (string) | poll 30s | 749 ns |
| `gcloud` | global | `project` (string), `account` (string) | poll 60s | 1.08 µs |
| `aws` | global | `profile` (string), `region` (string) | poll 60s | < 1 µs |
| `terraform` | path | `workspace` (string) | watch `.terraform/` | < 1 µs |

`kubecontext` reads `~/.kube/config` directly (respecting `$KUBECONFIG`) — no `kubectl` subprocess. `gcloud` reads `~/.config/gcloud/properties` directly — no Python CLI subprocess.

**Example output:**

```json
// comb get kubecontext
{
  "ok": true,
  "data": { "context": "prod-cluster", "namespace": "default" },
  "age_ms": 15200
}

// comb get aws
{
  "ok": true,
  "data": { "profile": "work-prod", "region": "us-east-1" },
  "age_ms": 42100
}
```

### Development Tools

| Provider | Scope | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `python` | path | `venv` (bool), `venv_name` (string), `version` (string) | watch `.venv/`, `pyproject.toml` | < 1 µs |
| `conda` | global | `env` (string), `version` (string) | poll 30s | < 1 µs |
| `mise` | path | `tools` (object: tool-name → version) | watch `.mise.toml`, `mise.toml` | varies |
| `asdf` | path | `tools` (object: tool-name → version) | watch `.tool-versions` | < 1 µs |
| `direnv` | path | `status` (string), `allowed` (bool) | watch `.envrc` | varies |

**Example output:**

```json
// comb get mise .
{
  "ok": true,
  "data": {
    "tools": {
      "node": "20.11.0",
      "python": "3.12.1",
      "rust": "1.75.0"
    }
  },
  "age_ms": 890
}

// comb get python .
{
  "ok": true,
  "data": { "venv": true, "venv_name": ".venv", "version": "3.12.1" },
  "age_ms": 120
}
```

---

## Custom Providers Guide

Custom providers let you add any data source to beachcomber using any language. Your script runs on the configured schedule, and the results are cached and served to all consumers.

### Output Formats

**JSON (default):** Stdout must be a JSON object. Top-level keys become provider fields.

```sh
# A provider that outputs JSON
#!/bin/sh
docker context show --format '{"context":"{{.Name}}","driver":"{{.Driver}}"}'
```

```toml
[providers.docker_ctx]
command = "~/.config/beachcomber/providers/docker-context.sh"
output = "json"
```

**Key-value:** Stdout is `key=value` lines, one per field. Simpler for shell scripts.

```sh
# A provider using kv output
#!/bin/sh
context=$(docker context show 2>/dev/null || echo "default")
echo "context=${context}"
```

```toml
[providers.docker_ctx]
command = "~/.config/beachcomber/providers/docker-context.sh"
output = "kv"
```

**Text:** Stdout is a single value, exposed as the `value` field. For commands that print one thing.

```sh
# Single-value output
node --version 2>/dev/null | tr -d 'v'
```

```toml
[providers.node_version]
command = "node --version | tr -d v"
output = "text"
```

Then query with `comb get node_version.value -f text`.

### Invalidation Strategies

**Poll only:** Re-run every N seconds. Use for data that changes independently of filesystem events.

```toml
[providers.vpn_status]
command = "~/.config/beachcomber/providers/vpn-check.sh"
output = "kv"

[providers.vpn_status.invalidation]
poll = "10s"
```

**Watch only:** Re-run when specific files change. Use for data that's determined entirely by file content.

```toml
[providers.ruby_version]
command = "rbenv version-name"
output = "text"
scope = "path"

[providers.ruby_version.invalidation]
watch = [".ruby-version", "Gemfile", ".tool-versions"]
```

**Watch with poll fallback (recommended):** FSEvents and inotify can occasionally drop events under heavy load. A poll fallback ensures eventual consistency even if an event is missed.

```toml
[providers.cargo_meta]
command = "cargo metadata --format-version=1 --no-deps --quiet"
output = "json"
scope = "path"

[providers.cargo_meta.invalidation]
watch = ["Cargo.toml", "Cargo.lock"]
poll = "120s"
```

### Real-World Examples

**Docker context provider:**
```sh
#!/bin/sh
# ~/.config/beachcomber/providers/docker-context.sh
# Outputs the active Docker context and whether it's remote.

context=$(docker context show 2>/dev/null || echo "default")
endpoint=$(docker context inspect "$context" --format '{{.Endpoints.docker.Host}}' 2>/dev/null || echo "")

is_remote="false"
case "$endpoint" in
    tcp://*|ssh://*) is_remote="true" ;;
esac

printf '{"context":"%s","remote":%s}\n' "$context" "$is_remote"
```

```toml
[providers.docker_context]
command = "~/.config/beachcomber/providers/docker-context.sh"
output = "json"

[providers.docker_context.invalidation]
poll = "30s"
```

Query: `comb get docker_context.context -f text`

**Node.js version provider (path-scoped):**
```sh
#!/bin/sh
# ~/.config/beachcomber/providers/node-version.sh
# Reports the Node.js version in effect for the current directory.
# Respects .nvmrc, .node-version, and volta/mise if installed.

if command -v mise >/dev/null 2>&1; then
    version=$(mise current node 2>/dev/null)
elif command -v node >/dev/null 2>&1; then
    version=$(node --version 2>/dev/null | tr -d v)
fi

echo "version=${version:-unknown}"
```

```toml
[providers.node_version]
command = "~/.config/beachcomber/providers/node-version.sh"
output = "kv"
scope = "path"

[providers.node_version.invalidation]
watch = [".node-version", ".nvmrc", "package.json", ".mise.toml"]
poll = "60s"
```

**Ruby version via rbenv:**
```toml
[providers.ruby_version]
command = "rbenv version-name 2>/dev/null || ruby --version | cut -d' ' -f2"
output = "text"
scope = "path"

[providers.ruby_version.invalidation]
watch = [".ruby-version", "Gemfile", ".tool-versions"]
poll = "120s"
```

Query: `comb get ruby_version.value -f text`

**VPN connected check:**
```sh
#!/bin/sh
# ~/.config/beachcomber/providers/vpn-status.sh
# Checks whether a VPN tunnel is active.

# Look for any utun interface with an IP (macOS)
if ifconfig 2>/dev/null | grep -q '^utun.*flags'; then
    # Check if a utun has an inet address (not just link-local)
    if ifconfig 2>/dev/null | awk '/^utun/{iface=$1} /inet / && iface{print; iface=""}' | grep -q inet; then
        echo "active=true"
        # Try to get VPN name from pf/scutil
        name=$(scutil --nc list 2>/dev/null | grep Connected | head -1 | sed 's/.*"\(.*\)".*/\1/')
        echo "name=${name:-vpn}"
        exit 0
    fi
fi

echo "active=false"
echo "name="
```

```toml
[providers.vpn]
command = "~/.config/beachcomber/providers/vpn-status.sh"
output = "kv"

[providers.vpn.invalidation]
poll = "10s"
```

Query: `comb get vpn.active -f text`

### HTTP Providers

For providers that fetch data from REST APIs, beachcomber has a built-in HTTP provider type. This makes HTTP requests directly in the daemon process — no `curl` fork, no shell spawning, with connection reuse and proper timeout handling.

> **Note:** You can also use script providers with `curl` for quick-and-dirty HTTP queries. But for anything polling regularly, the `http` type is significantly more efficient — it avoids 2-6ms of process spawn overhead per request.

**Basic API status check:**

```toml
[providers.claude_status]
type = "http"
url = "https://status.anthropic.com/api/v2/summary.json"
extract = "status"
invalidation = { poll = "60s" }
```

Query: `comb get claude_status.indicator -f text` returns `"none"`, `"minor"`, `"major"`, etc.

The `extract` field navigates into the JSON response using dot-separated paths. Without it, the entire response object becomes the provider's fields.

**Authenticated API with headers:**

```toml
[providers.github_rate]
type = "http"
url = "https://api.github.com/rate_limit"
headers = { Authorization = "Bearer ${GITHUB_TOKEN}", Accept = "application/json" }
extract = "rate"
invalidation = { poll = "30s" }
```

Query: `comb get github_rate.remaining -f text`

Header values support `${ENV_VAR}` expansion — secrets stay in your environment, not in config files.

**Service health endpoint:**

```toml
[providers.api_health]
type = "http"
url = "https://internal.example.com/health"
invalidation = { poll = "10s" }
```

If the endpoint returns JSON, top-level keys become fields. If it returns non-JSON, the raw body is available as the `body` field.

**Exchange rate (infrequent poll):**

```toml
[providers.exchange]
type = "http"
url = "https://api.exchangerate-api.com/v4/latest/USD"
extract = "rates.AUD"
invalidation = { poll = "86400s" }
```

Query: `comb get exchange.value -f text` — returns the AUD rate, refreshed daily.

**Comparison — script vs HTTP for the same task:**

Using a script provider (forks `sh` + `curl` every poll):
```toml
[providers.api_status_script]
type = "script"
command = "curl -s https://status.anthropic.com/api/v2/summary.json"
invalidation = { poll = "60s" }
```

Using the HTTP provider (in-process, no fork):
```toml
[providers.api_status_http]
type = "http"
url = "https://status.anthropic.com/api/v2/summary.json"
invalidation = { poll = "60s" }
```

Both produce the same result. The HTTP version skips the ~5ms process spawn overhead and handles connection failures more gracefully.

### Secrets and Environment Variables

HTTP headers and script commands support `${VAR}` expansion, pulling values from the daemon's environment. But the daemon's environment depends on how it starts — socket activation inherits the env of whatever triggered it, which is unpredictable.

The solution: **env files.** The daemon loads `~/.config/beachcomber/env` at startup before any providers execute, guaranteeing a consistent environment regardless of how the daemon was started.

```sh
# ~/.config/beachcomber/env
# This file is loaded by the daemon at startup.
# Format: KEY=VALUE (one per line). Blank lines and #comments are ignored.
# Values can be quoted: KEY="value with spaces" or KEY='single quoted'

GITHUB_TOKEN=ghp_xxxxxxxxxxxxxxxxxxxx
ANTHROPIC_API_KEY=sk-ant-xxxxxxxxxxxx
ANTHROPIC_ADMIN_KEY=sk-admin-xxxxxxxxxxxx
EXCHANGE_API_KEY=abc123
```

**Protect this file:**
```sh
chmod 600 ~/.config/beachcomber/env
```

Then reference these in provider configs:

```toml
[providers.github_rate]
type = "http"
url = "https://api.github.com/rate_limit"
headers = { Authorization = "Bearer ${GITHUB_TOKEN}" }
invalidation = { poll = "30s" }
```

The `${GITHUB_TOKEN}` is expanded at request time from the daemon's environment (which includes the env file values).

**Custom env file path:** If you keep secrets elsewhere:

```toml
[daemon]
env_file = "~/.secrets/beachcomber.env"
```

**Integration with secret managers:** Generate the env file from your secret manager of choice:

```sh
# 1Password
op read "op://Vault/beachcomber/env" > ~/.config/beachcomber/env

# pass
pass show beachcomber/env > ~/.config/beachcomber/env

# macOS Keychain
security find-generic-password -s beachcomber -w > ~/.config/beachcomber/env

# Vault
vault kv get -field=env secret/beachcomber > ~/.config/beachcomber/env
```

Then `chmod 600` and restart the daemon (`pkill -f 'comb daemon'` — it socket-activates on next query).

### Script Provider Tips

- **Exit codes:** A non-zero exit is treated as a failure. The last cached value is retained. After 3 consecutive failures, the provider enters exponential backoff (up to 60s).
- **Stderr:** Stderr output from script providers is captured and logged at `debug` level. It does not affect the result.
- **Timeouts:** Script providers are subject to `provider_timeout_secs` (default 10s). Long-running scripts are cancelled and retried on the next trigger.
- **Shell:** Commands are executed via `sh -c`. Use absolute paths for reliability, or ensure your PATH is set correctly in the daemon's environment.
- **Path-scoped providers:** If `scope = "path"`, the script is called with the directory path as its working directory. Use `$PWD` inside the script to reference it.
- **Performance:** Every process spawn costs 2-6ms minimum. For providers that poll frequently (< 30s), prefer reading config files over spawning CLI tools. See the design principles in `docs/performance.md`.

---

## CLI Reference

All commands are subcommands of `comb`. The daemon is socket-activated — you never need to start it manually.

### `comb get <key> [path] [-f format]`

Query a cached value. Returns immediately with cached data; never waits for a fresh computation.

```sh
# Query a specific field from a path-scoped provider
comb get git.branch /path/to/repo
comb get git.branch .          # relative path resolved to absolute

# Query a field from a global provider (no path needed)
comb get battery.percent
comb get hostname.short

# Query the entire provider (all fields)
comb get git .
comb get battery

# Output formats
comb get git.branch . -f text   # prints: main
comb get git.branch . -f json   # prints: full JSON response (default)

# Multi-field text output (key=value lines)
comb get git . -f text
# prints:
# ahead=0
# behind=0
# branch=main
# dirty=false
# staged=0
# stash=0
# state=clean
# untracked=2
# unstaged=0
```

**Exit codes:**
- `0` — success, data returned
- `1` — cache miss (provider has no data yet)
- `2` — error (daemon unreachable, unknown provider, invalid key)

### `comb poke <key> [path]`

Trigger immediate recomputation of a provider. Returns immediately after acknowledging the request — does not wait for the result. The next `get` will return the fresh value.

```sh
# Force git status refresh after a branch switch
comb poke git .

# Force network info refresh after connecting to VPN
comb poke network

# After modifying kubeconfig manually
comb poke kubecontext
```

**Exit codes:** `0` on success, `2` on error.

### `comb status`

Show daemon health and statistics.

```sh
$ comb status
{
  "uptime_secs": 7234,
  "cache_entries": 14,
  "active_watchers": 4,
  "providers": 16,
  "requests_total": 184291
}
```

### `comb list`

Show all active providers and their cached state age.

```sh
$ comb list
{
  "entries": [
    { "key": "git", "path": "/Users/me/project", "age_ms": 1240 },
    { "key": "battery", "path": null, "age_ms": 8900 },
    { "key": "kubecontext", "path": null, "age_ms": 22100 }
  ]
}
```

### `comb daemon [--socket <path>]`

Run the daemon in the foreground. You almost never need this — the daemon is socket-activated automatically. Use it for debugging or for running under a process supervisor.

```sh
# Run with debug logging
BEACHCOMBER_LOG=debug comb daemon

# Override socket path
comb daemon --socket /tmp/beachcomber-debug.sock
```

The daemon exits on SIGINT (Ctrl+C) with a graceful shutdown sequence.

---

## Debugging

### Log file

The daemon writes logs to `~/.local/state/beachcomber/daemon.log` (XDG state home). Both the foreground and background (socket-activated) daemon use this file. Logs are appended across restarts.

```sh
# Watch live daemon logs
tail -f ~/.local/state/beachcomber/daemon.log
```

### Changing the log level

The default log level is `info`. To enable debug logging, set `log_level` in your config:

```toml
# ~/.config/beachcomber/config.toml
[daemon]
log_level = "debug"
```

Valid levels: `trace`, `debug`, `info`, `warn`, `error`.

You can also override it at runtime using the `RUST_LOG` environment variable when running the daemon in the foreground (see below).

### Running the daemon in the foreground

The easiest way to watch what the daemon is doing is to run it interactively. Stop any running background instance first, then start it yourself:

```sh
# Kill the background daemon
pkill -f 'comb daemon'

# Run in foreground with debug logging
RUST_LOG=debug comb daemon

# Or use a custom socket to avoid interfering with your running setup
comb daemon --socket /tmp/beachcomber-debug.sock
```

Logs print directly to your terminal. Press Ctrl+C to shut down.

### Checking active state with `comb status`

`comb status` returns a JSON snapshot of the daemon's internal state:

```sh
comb status
```

```json
{
  "cache_entries": 3,
  "providers": 12,
  "watched_paths": ["/Users/you/project"],
  "in_flight": [],
  "backoff": [],
  "poll_timers": [
    {
      "provider": "battery",
      "path": null,
      "interval_secs": 30,
      "last_run_secs_ago": 12
    }
  ],
  "demand": [
    {
      "provider": "git",
      "path": "/Users/you/project",
      "last_query_secs_ago": 5
    }
  ]
}
```

Key fields:

- `watched_paths` — filesystem paths currently being watched for changes
- `in_flight` — providers currently executing (non-empty means a computation is running right now)
- `backoff` — keys in the drain/eviction sequence after demand expired
- `poll_timers` — active poll timers and when they last ran
- `demand` — providers kept warm by recent queries and when they were last queried

### Killing and restarting the daemon

The daemon will restart automatically the next time any client queries it (socket activation). To force a restart:

```sh
# Kill by PID file (socket path depends on your platform)
kill $(cat ~/.local/state/beachcomber/daemon.pid 2>/dev/null || \
       cat /run/user/$(id -u)/beachcomber/daemon.pid 2>/dev/null)

# Or by process name
pkill -f 'comb daemon'

# The daemon restarts automatically on next query
comb get hostname.short -f text
```

### Common issues

**Daemon never starts / connection refused**

The daemon socket path depends on `$XDG_RUNTIME_DIR` (Linux) or `$TMPDIR` (macOS). Check that the socket exists:

```sh
ls -la /run/user/$(id -u)/beachcomber/   # Linux
ls -la $TMPDIR/beachcomber-$(id -u)/     # macOS fallback
```

If the socket is missing, run `comb daemon` in the foreground to see why it failed to start.

**Provider always returns stale/empty data**

Check whether the provider is in a failure backoff loop:

```sh
comb status
# Look at the "backoff" field and the daemon log for "suppressed due to failure backoff"
```

Run the provider directly to check for errors:

```sh
# For git, run from inside a repo
comb get git .
tail -20 ~/.local/state/beachcomber/daemon.log
```

**High CPU or unexpected provider executions**

Enable debug logging and watch the log file. Look for repeated `Executed provider` lines:

```sh
RUST_LOG=debug comb daemon 2>&1 | grep 'Executed provider'
```

If a provider is executing too frequently, check whether a filesystem watcher is triggering on a high-churn path (e.g., a build output directory). Check `watched_paths` in `comb status`.

**Log file grows too large**

Logs are appended indefinitely. Rotate manually or add a logrotate rule:

```sh
# Truncate manually
: > ~/.local/state/beachcomber/daemon.log

# Or set a higher log level to reduce volume
# In ~/.config/beachcomber/config.toml:
# [daemon]
# log_level = "warn"
```

---

## Protocol Reference

beachcomber uses a simple newline-delimited JSON protocol over a Unix socket. Any language that can open a Unix socket and read/write JSON can be a client — no client library required.

### Connection

Socket path resolution order:
1. `daemon.socket_path` in config, if set
2. `$XDG_RUNTIME_DIR/beachcomber/sock`
3. `$TMPDIR/beachcomber-<uid>/sock`

Connect with `SOCK_STREAM`. Each message is a JSON object followed by `\n`. Each response is a JSON object followed by `\n`.

### Request Format

```json
{"op": "get", "key": "git.branch", "path": "/home/user/project"}
{"op": "get", "key": "git", "path": "/home/user/project"}
{"op": "get", "key": "battery"}
{"op": "get", "key": "git.branch", "path": "/home/user/project", "format": "text"}
{"op": "poke", "key": "git", "path": "/home/user/project"}
{"op": "context", "path": "/home/user/project"}
{"op": "list"}
{"op": "status"}
```

**Fields:**

| Field | Type | Description |
|---|---|---|
| `op` | string | Operation: `get`, `poke`, `context`, `list`, `status` |
| `key` | string | Provider name (`git`) or field path (`git.branch`) |
| `path` | string | Absolute path for path-scoped providers. Optional if connection context is set. |
| `format` | string | Response format: `"json"` (default) or `"text"` |

### Response Format

```json
{"ok": true, "data": {"branch": "main", "dirty": true}, "age_ms": 1240, "stale": false}
{"ok": true, "data": "main", "age_ms": 1240, "stale": false}
{"ok": true, "data": null, "age_ms": null, "stale": false}
{"ok": false, "error": "unknown provider: git2"}
```

**Fields:**

| Field | Type | Description |
|---|---|---|
| `ok` | bool | Whether the operation succeeded |
| `data` | any | Result: object (full provider), scalar (single field), or null (cache miss) |
| `age_ms` | int | Milliseconds since the cached value was last computed |
| `stale` | bool | Whether the value is past its expected refresh time |
| `error` | string | Error message when `ok` is false |

### Operations

**`get`:** Read from cache. Always returns immediately. If the key has never been computed, `data` is null and `ok` is true. A null response means "no data yet" — retry after a moment or `poke` to trigger computation.

**`poke`:** Trigger immediate provider recomputation. Returns `{"ok": true}` after acknowledging. The recomputation happens asynchronously — subsequent `get` calls will return the refreshed value once it completes.

**`context`:** Set the working directory for this connection. Subsequent path-scoped `get` requests without an explicit `path` will resolve relative to this directory. Useful for clients that query multiple values for the same path.

**`list`:** Returns an array of all active cache entries with their metadata.

**`status`:** Returns daemon health information.

### Text Format

When `"format": "text"` is specified:
- Single field queries return the raw value followed by `\n` (e.g., `main\n`)
- Full provider queries return `key=value` lines sorted alphabetically, one per line, terminated with `\n`
- Errors return nothing on stdout; `ok` is false in the JSON response

### Connection Context Example

```python
# Set context once, then query multiple values without repeating the path
sock.send(b'{"op":"context","path":"/home/user/myproject"}\n')
response = read_line(sock)  # {"ok": true}

sock.send(b'{"op":"get","key":"git.branch"}\n')
branch = read_line(sock)    # {"ok": true, "data": "main", ...}

sock.send(b'{"op":"get","key":"git.dirty"}\n')
dirty = read_line(sock)     # {"ok": true, "data": false, ...}
```

---

## Alternatives and Prior Art

beachcomber did not emerge from a vacuum. Several excellent tools have explored parts of this problem space. Here is an honest account of each and how beachcomber relates.

### gitstatusd (romkatv/gitstatus)

gitstatusd is the engine behind powerlevel10k and one of the fastest git status implementations in existence. On the Chromium repository (413k files), it returns results in 30ms — raw `git status` takes 295ms on the same repo.

gitstatusd's key insight was correct: a persistent daemon that maintains an in-memory cache of directory mtimes amortizes the cost of repeated git status queries. That insight is the foundation beachcomber builds on.

The limitation is architectural: gitstatusd spawns one daemon per interactive shell. On a machine with 20 shells open, that's 20 daemons, up to 640 threads, 20 independent FSEvents registrations all watching the same directories. The maintainer declined a shared-daemon proposal on security grounds, and powerlevel10k is now on maintenance-only status ("NO NEW FEATURES ARE IN THE WORKS. MOST BUGS WILL GO UNFIXED").

**beachcomber vs gitstatusd:** beachcomber is what gitstatusd would be if the daemon were shared across all consumers. One daemon, one cache, one watcher — for git and everything else. gitstatusd handles only git; beachcomber handles 16 providers plus extensibility. If you are a powerlevel10k user looking for a maintained, general-purpose replacement, beachcomber is the intended answer.

See `docs/competitive-landscape.md` for detailed numbers.

### Watchman (Meta/Facebook)

Watchman is a general-purpose filesystem watching daemon used by Jest, Buck, and Bazel. It is excellent at what it does: tracking file changes, maintaining an in-memory database of file metadata, and pushing events to subscribers via a rich expression language.

Watchman knows *that* files changed. It does not know *what a git branch is*, what battery percentage means, or how to assemble prompt data. It is plumbing, not porcelain.

beachcomber operates at a higher abstraction layer. The daemon internally uses the `notify` crate (which uses FSEvents/inotify directly) rather than depending on Watchman, keeping the dependency footprint small. A 88MB C++ daemon is a steep dependency for a prompt tool.

**beachcomber vs Watchman:** Complementary, not competitive. Watchman is infrastructure for build systems. beachcomber is a caching layer for shell state.

### powerline-daemon (powerline/powerline)

powerline-daemon is the conceptual ancestor of beachcomber. It was the original "cache prompt data in a daemon" approach — one daemon per user, Unix socket, serving shell prompts, tmux, and vim.

The architectural mistake: powerline-daemon cached the *rendering engine*, not the *data*. The daemon avoided re-parsing Python config files and re-importing modules on every prompt, but still invoked fresh subprocesses for git status, battery, and every other data source on every render. The 20-50ms per render that users experienced was entirely the subprocess overhead that the daemon failed to amortize.

powerline-daemon was also single-threaded, meaning one slow git segment on a monorepo would block all consumers. The last PyPI release was 2018.

**beachcomber vs powerline:** beachcomber is a direct correction of powerline's architectural decision. Cache the *data*, not the renderer. Compute once, serve many.

### Starship

Starship is the most popular cross-shell prompt with 55k stars. It is fast for typical repositories, with parallel module computation via rayon. It has no daemon, no caching, and no persistent state — each prompt invocation is a fresh process that computes everything from scratch.

On typical repositories starship completes in 1-5ms. On large monorepos it degrades significantly. Async git status — where the prompt renders immediately and git data fills in when ready — has been the most-requested feature since 2019 and has not shipped. The design space for a daemon has been explored (a detailed proposal exists from 2020) but has not been implemented.

beachcomber is the missing piece for starship. Using the `[custom.*]` module, starship can read pre-cached state from beachcomber instead of computing git/battery/hostname on every prompt. The latency drops from 5ms to 15µs for cache-warm queries.

**beachcomber vs Starship:** Not competitors — beachcomber is infrastructure that starship (and oh-my-posh, and p10k, and any other prompt framework) can use as a backend. The integration is already possible today via `comb get` in custom modules.

### Oh My Posh

Oh My Posh is a Go-based cross-shell prompt with TTL-based disk caching per segment. It is the closest existing approach to beachcomber's model within prompt tools: results can be cached to disk and reused within a TTL window.

The differences: disk-based (not memory), no daemon, no multi-consumer sharing, and TTL-based invalidation rather than filesystem-event-driven. A git status cached for 30 seconds might be shown stale after a `git checkout`; beachcomber would have invalidated and refreshed the cache immediately when `.git/HEAD` changed.

**beachcomber vs Oh My Posh:** beachcomber would give oh-my-posh users event-driven invalidation and cross-consumer sharing. The `[custom.*]` module approach works here too.

### direnv

direnv hooks into the shell's pre-prompt to manage directory-scoped environment variables. It uses mtime-based change detection on `.envrc` files and re-evaluates them when they change.

beachcomber's `direnv` provider wraps `direnv export json` and caches the result. Multiple consumers (different shell sessions, tmux panes) can see the direnv state through beachcomber without each running their own evaluation. This is the integration, not the replacement — direnv's evaluation semantics are preserved.

### The Gap

No single tool does all of this together:

| Capability | gitstatusd | Watchman | powerline | Starship | Oh My Posh | beachcomber |
|---|---|---|---|---|---|---|
| Shared daemon (one per user) | No | Yes | Yes | No | No | **Yes** |
| Caches interpreted state | Git only | No | No | No | TTL disk | **Yes (all)** |
| Multiple data types | No | No | Yes (recalc) | Yes (recalc) | Yes (recalc) | **Yes (cached)** |
| Multiple consumers | No | Yes | Yes | No | No | **Yes** |
| Event-driven invalidation | No | Yes | No | No | No | **Yes** |
| Extensible providers | No | N/A | Python only | TOML only | Go only | **Script + config** |

---

## FAQ

### Does beachcomber replace starship / powerlevel10k / oh-my-posh?

No. beachcomber is infrastructure — a data cache that prompt frameworks can consume. It does not render prompts, apply themes, or manage shell hooks. Think of it as a fast, shared data source that your existing prompt setup can optionally use instead of computing everything from scratch.

With beachcomber, starship reads git state from a cache instead of invoking gitoxide. With beachcomber, powerlevel10k (if it gains socket support) could share one gitstatusd-equivalent across all shells. The prompt frameworks stay; they just get faster.

### Why not just use Watchman?

Watchman tells you *which files changed*. beachcomber tells you *what the git status is*, *what the battery percentage is*, *which kubernetes context is active*. Watchman is a lower-level primitive — it produces events, not interpreted state.

Building on Watchman would mean beachcomber is also responsible for maintaining a Watchman installation, handling its failure modes, and adding 88MB+ to your system. The `notify` crate beachcomber uses talks to FSEvents/inotify directly, achieving the same result without the dependency.

### How much memory does the daemon use?

Light. The cache holds one result object per `(provider, path)` combination. A typical developer session with 10 active providers across 3 directories is around 30 cache entries. Provider results are small — the git state object is a few dozen bytes.

Unlike Watchman, beachcomber does not maintain an in-memory database of every file's metadata. It knows that `.git/HEAD` changed; it does not index every file in your repository.

On a system with 20 shells and typical usage, expect the daemon to use 10-30MB of RSS. The tokio thread pool is fixed-size; provider executions happen on `spawn_blocking` threads that are bounded by tokio's defaults.

### What happens when the daemon crashes?

The socket file is cleaned up on graceful exit. If the daemon crashes unexpectedly, the stale socket file may remain. The next client connection will attempt to connect, fail, detect the stale socket, remove it, start a fresh daemon instance, and retry. This is handled transparently — `comb get` will succeed with a slight delay on the restart.

You can verify the daemon is responsive at any time with `comb status`. If the daemon is unhealthy, `comb poke` on any key will trigger a restart if needed.

### Can I use this on Linux?

macOS is the primary target and the only supported platform for the current release. Linux support is designed in from the beginning — the filesystem watcher, battery reader, and network reader are all abstracted behind platform traits — and is planned for v0.2.0.

The providers that read config files directly (`kubecontext`, `gcloud`, `aws`, `conda`) work identically on Linux. The providers that use platform-specific APIs (`battery` via IOKit/pmset, `network` via getifaddrs + airport) will need Linux implementations reading `/sys/class/power_supply/` and `/sys/class/net/`.

### Can I run multiple daemons simultaneously?

The daemon is designed for one instance per user. Multiple daemon instances would each have independent caches and independent filesystem watchers, defeating the purpose of centralization. The socket activation logic prevents this by design: if a socket already exists and is responsive, the client uses it.

If you need per-project isolation (e.g., different config for work vs personal projects), use `daemon.socket_path` in a per-project config to run daemons on separate sockets.

### How do I add a provider for a tool beachcomber doesn't know about?

Write a script provider. See the [Custom Providers Guide](#custom-providers-guide). If the provider would be useful to everyone (not just your specific setup), consider contributing it as a built-in — see [Contributing](#contributing).

### What is the `stale` flag in responses?

Each provider has an expected refresh interval. If the cached value is older than that interval plus some tolerance, `stale: true` is set in the response. The value is still returned — beachcomber never blocks a read waiting for fresh data.

Consumers can use `stale` to decide whether to show a loading indicator or use a different visual style. For prompt use, ignoring `stale` is usually the right choice — showing a slightly old branch name is better than blocking the prompt.

---

## Contributing

beachcomber is in active development. See `CONTRIBUTING.md` for how to contribute, the PR process, and code standards.

For bugs, feature requests, and discussion, open an issue on GitHub.

If you are building an integration (a plugin for a prompt framework, an editor extension, a status bar module), the [Consumer Integration](#consumer-integration) and [Protocol Reference](#protocol-reference) sections have everything you need to get started. Integrations that live outside this repo are welcome — open an issue to get listed in the documentation.

---

*beachcomber is pre-1.0 software. The protocol wire format and config schema may change between minor versions before v1.0.0. See `docs/roadmap.md` for the stability timeline.*
