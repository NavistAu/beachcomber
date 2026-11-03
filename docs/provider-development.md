# Writing a Source

A provider is configuration. Every provider comb ships is a file, `defaults/<name>.toml`, written the way a user writes one, and any line of it can be overridden in `config.toml` or `conf.d/`. There is no other kind: to add a provider, write a Source. What jinja cannot do comes from a plugin (§8).

---

## 1. The model

### Three layers

- **Provider** — a namespace, named by the key used in `comb get <name>.<field>`. It holds one or more Sources. Its table is `[daemon.providers.<name>]`.
- **Source** — one schedule, one lifecycle, one set of fields: the unit of execution. Its table is `[daemon.providers.<name>.sources.<source>]`.
- **Field** — a value. Belongs to exactly one Source, unless the provider's `result` merges them in an order of its own.

A Source is up to four lines of jinja:

| line | says | sees |
|---|---|---|
| `root` | the directory an instance answers for (none: one instance, at `~`) | `env`, `_` |
| `data` | the body, rendered with the root as its working directory, on a blocking thread, so it may run a command | `env`, `comb`, `sources`, `daemon`, `_` |
| `result` | the fields, each one line over `data` (`result.<field>`), or one line whose object is the whole node | `data`, `_` |
| `return` | the schedule: `poll`, `watch`, `failback`, `budget`, `timeout`, `nearest` | |

A failing `data` or `result` line is a failure: nothing is written and the scheduler counts it toward `failback`. A `result` that renders nothing is a result: the Source has nothing to report here, and it replaces what the instance stored before, so a Source never leaves stale values behind by reporting nothing. A line that renders none leaves its field absent: `null` means absent, never `""`.

**`root`** says which instance answers for a directory. A Source whose records depend on a project (git, mise, direnv, asdf, terraform, python) walks up to its marker and renders that directory, or nothing when it finds none; no `root` line is a Source with no root: one instance, at `~`. A root equal to `$HOME` is the instance at `~`, and is not the same as finding nothing (see below). A line that fails is a root that could not be worked out, and it fails the read: it is never answered from the instance at `~`.

A Source's instances combine in one of two ways, set by `| nearest` on its `return` line:

- **Cascade** (the default). A read of `p.<key>` from directory `D` asks the nearest instance first (`R1 = root(D)`). If the key is missing there, it asks the parent of that root (`root(parent(R1))`), and so on, ending at the instance at `~` and never past it; finding no root continues to `~`. A namespace read takes every instance on the way and merges them, the nearer winning per leaf. That is how `asdf` is a single Source: inside a project `asdf.node` is the project's `.tool-versions`, and `asdf.go`, which only `~/.tool-versions` sets, is found at `~`.
- **Nearest.** The instance `root(D)` finds answers alone. A key it lacks is absent: no outer instance fills in, and the walk does not move up. If no root is found there is no instance at all: nothing runs and nothing is read, and `~` is not a fallback. Finding `$HOME` itself is an instance (the one at `~`). That is how `git` works: inside a nested repository `git.branch_parent` is the inner repository's or absent, never the superproject's, and in a directory outside any repository there are no git values.

Choose nearest when the instance is *the one you are in* (git, direnv, a Python venv, a terraform root); keep cascade when instances are layers of one setting (mise, asdf's `.tool-versions`). A Source with no root has its one instance at `~` either way. Records written by `put`, and the `config:` seeds, merge up the directory chain whatever a Source does.

An instance's lifecycle key is `(provider, root, source)`, with no root for the instance at `~`. Its schedule registers when the instance is first requested, never at load: relative `watch` paths are watched under the instance's root, and absolute ones whatever the root. A relative `watch` on a Source with no root is a load error: it has no directory to resolve it against.

---

## 2. Writing one

### 2.1 Decide on Sources

Answer these for each group of fields; a group that differs is another Source, so a cheap field need not run with a dear one:

1. How do these fields change? (a file, a timer, or both)
2. Does it have a root? (none: one instance, at `~`; a root: one instance per project root. Then: do they cascade, or does the nearest answer alone?)
3. What can it read rather than run?

| Signal | Root | `return` |
|---|---|---|
| File changes only | with or without | `watch(...)` |
| Timer only | with or without | `poll(...)` |
| File changes + safety timer | with or without | `watch(...) \| poll(...)` |
| A file outside any project | none | `watch('/absolute/path')` |

A Source with `watch` and no `poll` runs on first demand and again only when a watched path changes. Like every Source it decays once nothing reads it (its Active window is the default poll lifespan, 60 s × 12) and its watches are dropped; the next read runs it again. There is no spelling for "run once": the shipped `hostname`, `user` and `uname` poll every 10 minutes, 10 minutes and hour while read, so a renamed machine is seen.

### 2.2 Write the file

In `~/.config/beachcomber/conf.d/docker.toml`, say. The current Docker context, read from Docker's own file with no process:

```toml
[daemon.providers.dockercontext.sources.config]
# No root: one instance, at ~, so only absolute paths can be watched.
data = "{{ '~/.docker/config.json' | read | fromjson }}"
result.name = "{{ (data.currentContext or 'default') if data is not none }}"
return = "{{ result | watch('~/.docker/config.json') | failback('60s', 3) }}"
```

A missing file reads as undefined and every line over it renders nothing, so with no Docker config the Source reports nothing, which is not a failure.

### 2.3 Use it

```bash
comb get dockercontext.name
comb check config      # lints every line, and names the file that set it
```

An edit under `providers` reloads in place. Read a shipped file to see how one is written: `defaults/python.toml` is `root` and `data` with some logic, `defaults/hostname.toml` one plugin call, `defaults/git.toml` all of it.

### 2.4 Worked example: git

`git` is `defaults/git.toml`, five Sources with different strategies, sharing one `root` line in their `_` prototype. Four get their data from the compiled-in `git` plugin (§8):

```
git.head    → watch, rooted at the repository, watches .git/ (reads <gitdir>/HEAD itself, no subprocess)
              fields: branch, detached

git.refs    → watch, rooted at the repository, watches .git/, `plugin('git', 'refs', root)`
              fields: commit, tag, ahead, behind, upstream, state, state_step, state_total,
                      stash, last_commit_age_secs, commit_summary, push_ahead, push_behind

git.diff    → poll 30s, rooted at the repository, `plugin('git', 'diff', root)`
              fields: lines_added, lines_removed, lines_staged_added, lines_staged_removed

git.lineage → poll 60s, rooted at the repository, `plugin('git', 'lineage', root)`
              fields: branch_parent, lines_added_parent, lines_removed_parent,
                      branch_default, lines_added_default, lines_removed_default

git.status  → watch + poll (watches .git/index + polls every 60s), rooted at the repository, `plugin('git', 'status', root)`
              fields: staged, unstaged, untracked, conflicted, dirty
```

This decomposition allows git branch lookups (hot path, watch-driven) to stay fresh without dragging along the slower diff computation.

Consumers use `comb get git.branch .`: the registry's `field → source` map routes the query to the `head` Source.

---

## 3. Data providers and template-backed namespaces

Some providers serve raw data sets that clients consume through templates. The pattern separates two concerns:

- **Data provider** — a daemon provider that caches all variants of an env-selected source. Its name is descriptive of the data it holds (e.g. `aws_profiles`, `gcloud_configs`). Each field is a variant keyed by name, each value an object of per-variant fields. The daemon caches the whole set regardless of any shell selector.
- **Consumer namespace** — a `[client.providers.<ns>] template.*` table, shipped as a managed `defaults/<ns>.toml` file, whose templates present the user-facing surface (e.g. `aws`, `gcloud`). These reference `comb.<data_provider>[<selector>].<field>` to index the data provider by the active selector; the daemon serves the templates as data and the client renders them with the caller's environment.

```
# data provider: daemon-side, env-blind
aws_profiles.default  → Object{ region: "us-east-1" }
aws_profiles.staging  → Object{ region: "us-west-2" }

# consumer namespace: templates in defaults/aws.toml, rendered by the client
[client.providers.aws]
template.profile = "{{ env.AWS_PROFILE or env.AWS_VAULT or env.AWS_DEFAULT_PROFILE or 'default' }}"
template.region = "{{ env.AWS_REGION or env.AWS_DEFAULT_REGION
              or comb.aws_profiles[ env.AWS_PROFILE or env.AWS_VAULT
                                     or env.AWS_DEFAULT_PROFILE or 'default' ].region }}"
```

`comb get aws_profiles` returns the raw dump. `comb get aws` renders the consumer templates. The daemon never reads `$AWS_PROFILE`.

### The `comb.*` reference convention

Inside value expressions, `comb.<provider>.<field>` addresses the **raw cached value**, bypassing any field expression. This is distinct from `<provider>.<field>`, which evaluates that field's value expression (recursive, cycle-detected).

This matters when a template needs to override a same-named cached value — a template at a cached field's path wins on read:

```
# defaults/terraform.toml: the template at terraform.workspace overrides the cached workspace without a cycle
[client.providers.terraform]
template.workspace = "{{ env.TF_WORKSPACE or comb.terraform.workspace }}"
```

`comb.terraform.workspace` is the stored value; `terraform.workspace` is the resolved field. No self-reference, no rename needed.

---

## 4. What a read checks

A read answers from a Source's stored record only while the record is valid: within its lifetime (its poll), and with its **stat inputs** as its run found them. A run stamps its inputs immediately before it reads them, and a read stats them again (one `stat(2)` per input, on the request task) before it answers. If one changed, the record's instance runs again, and the read waits up to the Source's budget for it. A change while the run executes fails the next read's check, so it costs one run more, never a stale answer.

A record's inputs are the paths its `watch(...)` declares (each relative one joined to the instance's root, each absolute one as written, and at the `~` instance each single-component one under `$HOME`) and the files its `data` and `result` lines read. A path the lines compute (a linked worktree's `HEAD`, a file named by `env.AWS_CONFIG_FILE`, a hash) is checked as a constant one is: nothing needs declaring. A Source that only polls and reads no file has none, has no validity, and is checked for its lifetime alone.

| item | what it reads |
|---|---|
| `p \| read`, `p \| stat`, `p \| realpath` | the path |
| `lup(name)` | every path its walk probed: `<dir>/<name>` from the instance's directory up to the hit (to the boundary when there is none, or with `all`); with `git=true`, each `<dir>/.git` it read to find the repository's root. A miss is read too: a `name` made nearer is a change |
| `pattern \| glob` | each directory it listed, as a node (an entry made, removed or renamed in it is a change), and each literal component it found missing or ended on. A directory passed through on the way is not: what the glob read below it is |

```toml
[daemon.providers.flag.sources.s]
root   = "{{ lup('.flagroot') | dirname }}"
data   = "{{ lup('state.json') | read }}"     # checked: every state.json probe, and the one read
result.v = "{{ data }}"
return = "{{ result | poll('10m') }}"           # no watch() needed for a read to see a rewrite
```

- The first 64 files a run reads are checked; a run that reads more logs once per Source (`flag.s: 150 files read; the first 64 are checked on read`), and a change to a later file waits for the watch or the poll.
- What an `exec` child or a plugin reads is not the lines' reading: a Source that shells out declares what its command reads with `watch(...)`, or `stat`s it in its own line (as `git`'s `refs` and `status` do), or lives with its poll.
- The `root` line's walk is not a record's input. The root a directory finds is kept per directory (see "How config Sources work").
- A `watch(...)` path is still what the daemon watches in the background, to warm a record before anyone reads it; what the lines read is checked on a read only.

- **A stamp** is `dev`, `ino`, size, mtime in nanoseconds and kind, following
  symlinks, plus the link's own inode and mtime for a symlink. An absent path
  has a stamp of its own, so creating an input is a change. A path that cannot
  be stat'd (`EACCES`) is unknown, and keeps the record.
- **A directory** is an input as a node, by its own mtime: an entry created,
  removed or renamed in it is a change (git writes `HEAD` and `index` by
  renaming a lockfile in `.git/`); a change deeper in the tree is left to the
  watch.
- **A network or FUSE mount** (NFS, SMB, AFP, FUSE, or one not known) is never
  stat'd on a read: a stat there can hang and its attribute cache can lie. A
  record with an input there is checked for its lifetime only, and the daemon
  logs one warning per mount.
- **A racy stamp**, taken within the mount's timestamp resolution of the
  file's mtime (and at least 10 ms), cannot see a same-size write in place in
  that window. The first read once the window has closed counts the record
  invalid and runs it again.

Name only files the run reads: an input that changes when the value does not costs a run for nothing. A directory is the blunt case: `watch('.git')` and `lup('.git')` make `.git` an input, whose mtime moves whenever a git command makes a lockfile there, so the next read runs the Source again. There is no way yet to watch a directory without also checking it on a read.

The record's validity hash is shown as `hash` in its `:cache` answer
(`comb get git:cache.branch`), 16 lowercase hex digits, written by the daemon
alone.

---

## 5. The schedule

A Source's `return` line maps onto one of three strategies:

| Signals available | Line |
|---|---|
| Timer only, no watchable file | `poll(interval, count)` |
| Filesystem events only, no poll needed | `watch(paths...)` |
| Filesystem events + safety backstop timer | `watch(paths...) \| poll(interval, count)` |

**`poll(interval, count)`** runs the Source on a timer; the entry stays Active for `count` polls (`interval × count`) after its last read. Use it when there is no file that reliably reflects state changes.

```toml
return = "{{ result | poll('30s', 4) }}"      # battery level: no file to watch reliably
```

**`watch(paths...)`** runs the Source when a path changes. A relative path is under the instance's root (Sources with a root only). An absolute or `~` one is watched directly, whatever the root. Its arguments are constants: a path a line computes is checked on a read (§4), not watched.

```toml
return = "{{ result | watch('.git') | nearest }}"    # git.refs: any change under .git/, per repository
```

There is no automatic poll fallback for a `watch`. If a watch cannot register at runtime, the scheduler logs a warning and the Source serves its last cached value. If a backstop is needed, add a `poll`, as `git.status` does for what does not touch a watchable file:

```toml
return = "{{ result | watch('.git/index') | poll('60s', 2) | failback('30s', 3) | nearest }}"
```

A `return` names what it wants, and the line is whole: `nearest` too, so a retune of a nearest Source says `| nearest` again or the Source cascades. A failback, budget or timeout it does not name is the engine's (`failback('1s', 3)`, `[daemon] budget`, `[daemon] timeout`), and a Source with no `return` takes the nearest `_`'s (the abstract Source's `poll('60s', 12) | failback('1s', 3)` ships in `defaults/00-comb.toml`). `| super` extends the `_` line instead of replacing it.

A shipped Source is retuned by setting that line in a later layer, and any other line of it the same way:

```toml
[daemon.providers.git.sources.status]
return = "{{ result | watch('.git/index') | poll('120s', 6) | failback('2s', 5) | nearest }}"
```

Three things a Source relies on beyond its own table:

- **Prototypes.** A table named `_` (`[daemon.providers.<p>.sources._]` for one provider's Sources, `[daemon.providers._.sources._]` for all) holds defaults. A Source's own `root`, `data`, `result` or `return` line wins whole; an absent line takes the nearest `_`'s; `| super` on a `return` extends the `_` line instead of replacing it. The abstract Source (`poll('60s', 12) | failback('1s', 3)`) ships in `defaults/00-comb.toml`.
- **The engine.** `data` can call `exec`, `url`, `plugin`, `read`, `stat`, `lup`, `fromjson`, `fromini`, `merge`, `map` over objects, `pluck`, `dirname` and `tojson` (the table is in the configuration reference). An `exec` string is run by `sh -c`, so `$BAR` in it is invisible to the engine, and to a future `cache()` hash of the line: prefer `exec('cmd ' ~ env.bar)`.
- **Calling a Source.** `sources.<s>()` reads a sibling's stored record and `daemon.providers.<q>.sources.<s>()` any provider's. A Source with nothing stored is run inline, once, by the read or refresh that needs it (a cold call); a ring of Sources calling each other is a load error; a called Source stays warm and decays with its callers.

A provider is switched off with `[daemon.providers.<p>] enabled = false`: no Source of it runs, a read is a miss with no template half, and puts under it stay readable. An edit under `providers` reloads in place; only the bare `[daemon]` and `[client]` keys restart the daemon. Decay depth is data (`[daemon.providers._.cache._] decay = 4` ships), settable per provider or by `put`.

The root a directory finds is kept: a `root` line (it may run a command) is rendered once per directory rather than per request. The kept answer carries the stamps of the directories from the one asked about up to the root it found (up to `$HOME`, or `/` outside it, when it found none), taken before the line rendered, and a read checks them, one `stat` per directory: creating or removing a marker in one moves that directory's mtime, so the next read renders the root again. A marker created below a kept root, or the marker at it removed, is seen at once, and so is one that gives a `| nearest` Source that found nothing its instance. A change the stamps cannot see (what a command in the line reads, a file rewritten in place) is seen when the instance the root named is evicted (it has gone cold and decayed away), which drops the kept answer too. A `root` line that fails is answered with the same error for a few seconds rather than re-run on every request.

### Cache lifecycle

The full decay model (state machine, step durations, reinstatement rules) is described in `docs/architecture.md` §5. A Source's watches exist only while its instance is Active: decay drops them and the read that reinstates the instance registers them again, so a decaying record is checked by stat (§4) rather than by an event.

**Decay depth is data.** After the Active window an entry steps through its `decay` steps (4 unless told otherwise; step n lasts `lifespan × 2^n`) and is then evicted. A depth is put like any value: `comb put '<provider>:cache._.decay' 6` for one provider, `comb put '*:cache._.decay' 3` for every provider, with `--path` to scope it to a directory and below; the nearest prototype wins, and a lifecycle fixes its depth when it registers. `comb get <provider>:cache.<field>` shows the `poll`, `watch` and `decay` an entry registered with. There is no per-source keep-alive duration: `fsevent_lifespan` was removed, and a watch-only Source's Active window is the default poll lifespan (60 s × 12).

---

## 6. Performance

A Source runs on tokio's blocking thread pool. A slow one delays cache freshness but does not block the scheduler loop. Still, keep them fast. The tier list from `docs/performance.md`:

| Tier | Target | Method |
|---|---|---|
| Nanosecond (<1µs) | `user`, `hostname`, `gcloud`, `aws` | libc calls, env vars, file reads + line scan |
| Microsecond (1-100µs) | `terraform`, `python`, `direnv` (no binary) | File existence checks + small reads |
| Millisecond (1-10ms) | `network`, `battery` | At most one process spawn |
| Slow (10-50ms) | `mise`, `direnv` (with binary), config Sources that shell out | Multiple spawns or interpreted CLI |
| A repository's worth | `git` (about 150 ms for all five Sources on this repository, one after another) | One `git` process per fact; `head` none |

**Rule 1: Never run a process when you can read a file.**

A process costs 2-6 ms at least; a file read costs microseconds. Before reaching for `exec()`, ask: does this tool write its state to a file I can parse?

```toml
# Bad: a process for a value that is on disk
data = "{{ exec('gcloud config get-value project') }}"
# Good: read the file the tool reads
data = "{{ '~/.config/gcloud/configurations/config_default' | read | fromini }}"
```

Real examples from `docs/performance.md`:
- `gcloud`: reading `~/.config/gcloud/properties` instead of spawning the Python CLI — 500ms to 1µs (~500,000x)
- `git` stash: reading `.git/logs/refs/stash` instead of `git stash list` — 5ms to 1µs
- `git`'s `head`: reading `<gitdir>/HEAD` in the file's own line, so `git.branch` costs no process at all

**Rule 2: If you must run a process, run as few as you can.** `git`'s `status` Source runs one (`git status`), which is why it is its own Source: a read of `git.dirty` does not pay for `git.lines_added`.

**Rule 3: What polls often must be fast.** A Source polling every 5 seconds and taking 50 ms per run consumes 1% of a blocking thread slot continuously. Match the interval to the cost:
- Sub-microsecond: can poll every 5-10s safely
- Millisecond: 30s minimum
- Slow (>10 ms): 60s minimum, or `watch` instead

**Rule 4: A plugin symbol must be reentrant, or the library is locked.** A library on the search path is entered by one thread at a time. A compiled-in plugin takes no lock, so its symbols keep nothing between calls.

See `docs/performance.md` for the full performance profile, benchmark commands, and the regression checklist.

---

## 7. Testing

A shipped file is tested as a user reads it: `tests/suite/shipped/<namespace>.rs`, through `tests/suite/shipped/mod.rs`. `scenario()` is a daemon in the test's process over a temp `HOME` and `XDG_CONFIG_HOME`, which writes the managed defaults as `comb daemon` does; `served(vars, conf)` is the same shipped registry behind a server with no scheduler, so a read that answers a changed file found the change itself; and a Source can be run straight, `with_shipped_defaults().source(p, s).execute(dir)`, which needs no daemon and costs no socket.

```rust
#[test]
fn terraform_answers_nothing_outside_a_project() {
    let s = scenario();
    assert_eq!(s.get("terraform.workspace", &s.project), None);
}
```

Set up what the Source reads in the scenario's directory (a repository with `GitRepoFixture`, a `.tool-versions`, a `pyvenv.cfg`), read it, and assert on the JSON. Where a value depends on the machine (the hostname, the load), assert the shape and the type. Where a tool may be absent, put a stand-in on the isolated `PATH` (`TestDaemon::spawn_with_tools`).

What a compiled-in plugin does is unit tested beside it (`src/plugin/`): its parsers over canned output, through a seam like `GitExecutor`, cost no process. A test of the daemon that needs a Source of its own (a gated mock, a counter) implements the `Source` trait and registers it with `ProviderRegistry::register_sources`, gated behind `cfg(test)` and the `test-helpers` feature.

### Platform differences

Some facts come from the platform differently. The shipped `battery` and `network` providers show the pattern: one Source, with no platform branch in its file, whose `data` line calls a `sys` symbol that hides the difference. `src/plugin/sys/mod.rs` holds what is shared and dispatches to `sys/macos.rs` or `sys/linux.rs` through one `#[cfg]` alias; the platform modules expose `pub(super)` functions.

For testing, keep the platform's text handling pure and put it in `sys/parse.rs`, compiled `#[cfg(any(target_os = "...", test))]`, so the Linux parsers are tested on macOS and the reverse; gate only what reads the live system with `#[cfg(target_os = "...")]`.

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

### `sys`: the plugin compiled in

What only the operating system can say comes from one plugin compiled into `comb`, `sys`, called as any plugin is:

```toml
[daemon.providers.box.sources.load]
data = "{{ plugin('sys', 'loadavg') }}"
result.one = "{{ data.one }}"
return = "{{ result | poll('10s') }}"
```

Its symbols are the C ABI's own (`const char* symbol(const char* input)`, JSON in and out, `NULL` for failure), held in a table in the binary: `plugin('sys', …)` takes the same path from the engine function to the C call as a library's symbol does, and only the finding of the function differs (the table, not `dlsym`). Each takes no arguments, answers an object, and leaves out a fact it has no value for, never `""`.

| symbol | answers | from |
|---|---|---|
| `hostname` | `{name, short}` | `gethostname`; `short` is the first label |
| `loadavg` | `{one, five, fifteen}` | `getloadavg` |
| `uname` | `{sysname, release, version, machine}` | `uname` |
| `uptime` | `{seconds}` | macOS `sysctl KERN_BOOTTIME`; Linux `/proc/uptime` |
| `user` | `{name, uid}` | `getuid`, `getpwuid_r`; `name` is absent for a uid with no entry |
| `network` | `{interface, ip, ipv6, vpn_active, vpn_name, ssid, online}` | `getifaddrs`; the preferred interface is an `en*` one on macOS and the default route's (`/proc/net/route`) on Linux; the SSID from `airport` on macOS (gone from macOS 14.4, where it is absent) and `nmcli`, then `iw`, on Linux |
| `battery` | macOS `{percent, charging, status, time_remaining_secs}`; Linux `{percent, charging, status_raw, status, time_remaining_secs}`; `{}` with no battery | macOS `pmset -g batt`; Linux sysfs (`capacity`, `status`, and the estimate from `time_to_empty_now`/`time_to_full_now` or the energy or charge over the rate) |

The name is reserved: `plugin('sys', …)` never searches `[daemon.plugin] path`, and `comb check config` warns of a `libsys.dylib` or `libsys.so` there, which nothing would load. An unknown symbol is `plugin(): <symbol> in sys: symbol not found`; a symbol given arguments returns `NULL`.

The shipped `hostname`, `user`, `uname`, `load`, `uptime`, `battery` and `network` providers are Sources like the one above, each in its own `defaults/<name>.toml`: read one there to see how it is written, and override any of its lines in `config.toml` or `conf.d/`.

### `git`: the plugin compiled in for repositories

`git` is the second compiled-in plugin, by the same table and the same one call path as `sys`. What git says through its own commands needs an environment set, commands that fail in a healthy repository tolerated, and `lineage`'s branching, so it is a plugin and no shell runs on a refresh. Each symbol takes one argument, the work tree's root, and answers an object: a missing fact is left out, never `""`, and a directory that is not a work tree's root answers `{}`.

| symbol | answers | runs |
|---|---|---|
| `refs` | `{upstream, ahead, behind, stash, state, state_step, state_total, commit, last_commit_age_secs, commit_summary, tag, push_ahead, push_behind}` | `git status`, `log -1`, `describe`, `config` twice; with a push remote `rev-parse` and `rev-list` |
| `diff` | `{lines_added, lines_removed, lines_staged_added, lines_staged_removed}` | `git diff --numstat`, `diff --cached --numstat` |
| `lineage` | `{branch_parent, lines_added_parent, lines_removed_parent, branch_default, lines_added_default, lines_removed_default}` | `git rev-parse`, `merge-base`, `diff --numstat` |
| `status` | `{staged, unstaged, untracked, conflicted, dirty}` | `git status` |

Every command runs with `GIT_OPTIONAL_LOCKS=0`, so `git status` cannot rewrite the index its Source watches, `GIT_TERMINAL_PROMPT=0` and the askpass guards, and with `GIT_DIR`, `GIT_COMMON_DIR` and `GIT_WORK_TREE` removed. That environment is in one place in Rust (`src/boundaries/git.rs`), so no Source can forget it. `NULL` is failure: `git status` failing where a symbol needs it, or arguments other than one string.

`git` is reserved as `sys` is: `plugin('git', …)` never searches `[daemon.plugin] path`, and `comb check config` warns of a `libgit.dylib` or `libgit.so` there.

The shipped provider is `defaults/git.toml`, five Sources over those symbols, `head` reading `<gitdir>/HEAD` itself with no process. A `data` line names its directory the way its `root` line does, `lup('.git', root=true) | dirname`: a line renders in the instance's root, so that is `lup`'s first probe. A warm read cannot see a file a plugin reads, so what must be seen by the next read is `stat`ed in the Source's own line: `<commondir>/packed-refs` and `<commondir>/refs/heads/<branch>` for `refs`, `<gitdir>/index` for `status`. A user can override any line of the file, but not copy or edit what a symbol does.
