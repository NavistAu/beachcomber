---
sidebar_position: 3
title: Configuration Reference
---

# Configuration Reference

beachcomber runs with sensible defaults and requires no configuration. The optional config file lives at `~/.config/beachcomber/config.toml`.

The file has two roots. `[daemon]` is the daemon's: its settings (`socket_path`, `log_level`, `plugin.path`, `timeout`, `budget`, ...) and `providers`, where a provider's Sources, its `template`/`result`/`return` lines, its `cache` (decay) and `enabled` live. `[client]` is what clients render: `providers.<p>.template`. A provider exists if either root names it; one named only in a root is a provider with no Sources. Nothing else is a root: `[providers]`, `[lifecycle]`, `[failback]` and `[tree]` are errors.

A provider is a namespace made of **Sources**. A Source is a jinja function: `root` says which directory an instance answers for, `data` runs on the daemon's engine (below) and gets its body, `result` shapes that into fields, and `return` says when it runs again (`poll`, `watch`, `ttl`, `failback`, `budget`, `nearest`). A table named `_` is a prototype: a Source's own line wins whole, an absent line takes the nearest `_`'s, and `| super` extends it instead. The abstract provider and the abstract Source ship in `defaults/00-comb.toml`, so a Source you write says only what differs.

```toml
# ~/.config/beachcomber/config.toml

# ─── Daemon ────────────────────────────────────────────────────────────────────

[daemon]

# Override the Unix socket path.
# Default resolution order: BEACHCOMBER_SOCKET env var →
#          /tmp/beachcomber-<uid>/sock
# socket_path = "/path/to/socket.sock"

# Log level for daemon output.
# Options: "error", "warn", "info", "debug", "trace"
# Default: "info"
# Logs go to: $XDG_STATE_HOME/beachcomber/daemon.log
log_level = "info"

# How long one run of a Source may take before it is cut off, for every
# Source whose `return` does not say `timeout(...)`. The run fails and the last
# good cached value is retained. Any duration of at least 1ms.
# Default: "10s"
timeout = "10s"

# Path to an environment file loaded at daemon startup.
# Each line is KEY=VALUE (or KEY="VALUE"). Blank lines and #comments are ignored.
# These vars reach a Source's lines as `env.VAR` (an unset one renders empty) and
# the commands its exec() runs.
# Default: ~/.config/beachcomber/env (loaded automatically if present)
# env_file = "~/.config/beachcomber/env"

# How often the watchdog checks the scheduler heartbeat.
# If the heartbeat hasn't advanced within the threshold, the daemon shuts down
# for the process supervisor (launchd, systemd) to restart.
# Default: disabled (no watchdog)
# watchdog_interval = "30s"

# How long the heartbeat can be stale before the watchdog triggers shutdown.
# Default: 3x watchdog_interval
# watchdog_threshold = "90s"

# How long the daemon waits with no active connections before shutting
# itself down. The next client connection will socket-activate a fresh
# instance. Whole seconds, at least 1s.
# Default: unset (daemon stays resident)
# idle_shutdown = "5m"

# How long a read waits for a Source before answering with what it has, for
# every Source whose own `return` does not say `budget(...)`. Any duration of
# at least 1ms. (Recorded, not yet enforced: a later release reads it.)
# Default: "50ms"
# budget = "50ms"


# ─── Retuning a built-in Source ────────────────────────────────────────────────
# A built-in Source takes `return` and nothing else. The line replaces its
# schedule whole: here git's diff polls every 15 seconds, eight times, then
# decays. A built-in with no `return` here keeps its own schedule. The line
# replaces everything it declares, `nearest` included: git, direnv, python and
# terraform Sources are nearest, so a retune of one says `| nearest` again.

[daemon.providers.git.sources.diff]
return = "{{ result | poll('15s', 8) | failback(5, '2s') | nearest }}"


# ─── Config Sources ────────────────────────────────────────────────────────────
# Define your own providers as Sources: `data` runs a command, `result` shapes
# its output into fields, `return` says when it runs again.

# Minimal: a global provider that polls every 30 seconds
[daemon.providers.docker_context.sources.main]
data = "{{ exec('docker context show') | trim }}"
result.value = "{{ data }}"            # one field, `docker_context.value`
return = "{{ result | poll('30s') }}"

# A path-scoped provider that watches files and has a poll fallback
[daemon.providers.node_version.sources.main]
root = "{{ lup('package.json') | dirname }}"   # the directory an instance answers for
data = "{{ exec('node --version') | trim }}"
result.value = "{{ data }}"
return = "{{ result | watch('.node-version', '.nvmrc', 'package.json') | poll('60s') }}"

# A Source with structured JSON output: `result = "{{ data }}"` owns the whole node
[daemon.providers.cargo_meta.sources.main]
root = "{{ lup('Cargo.toml') | dirname }}"
data = "{{ exec('cargo metadata --format-version=1 --no-deps --quiet') | fromjson }}"
result = "{{ data }}"
return = "{{ result | watch('Cargo.toml', 'Cargo.lock') | poll('120s') }}"


# ─── HTTP ─────────────────────────────────────────────────────────────────────
# Fetch data from REST APIs with url() in a Source's data line — no curl fork,
# no shell spawning. A non-2xx response fails the Source (its failback applies).

# Basic: poll a status API
[daemon.providers.service_status.sources.main]
data = "{{ url('https://status.anthropic.com/api/v2/summary.json') | fromjson }}"
result.indicator = "{{ data.status.indicator }}"   # response.status.indicator → field "indicator"
return = "{{ result | poll('60s') }}"

# With auth headers (the daemon's environment is `env`)
[daemon.providers.github_rate.sources.main]
data = "{{ url('https://api.github.com/rate_limit', headers={'Authorization': 'Bearer ' ~ env.GITHUB_TOKEN}) | fromjson }}"
result = "{{ data.rate }}"                          # { "limit": 5000, "remaining": 4999, ... }
return = "{{ result | poll('30s') }}"

# Infrequent poll (daily)
[daemon.providers.exchange_rate.sources.main]
data = "{{ url('https://api.exchangerate-api.com/v4/latest/USD') | fromjson }}"
result.value = "{{ data.rates.AUD }}"               # a single nested value
return = "{{ result | poll('86400s') }}"
```

### The engine — what a Source's lines can call

A Source's `data` and `result` lines run on one jinja engine in the daemon. `env` is the daemon's own environment (its `env_file` included, never the caller's), `comb.<provider>` reads the provider as a `get` of it would — put over source, through its `return` line, a cold Source run first, once (a Source never waits on itself: one that reads its own provider finds its own last stored value) — and these functions and filters are added to minijinja's built-ins. A Source's `return` is not on this engine: it sees `result` and the schedule decorators (`poll`, `watch`, `ttl`, `failback`, `budget`, `timeout`, `nearest`, `super`) alone.

| Name | What it does |
|---|---|
| `exec(cmd, timeout='10s', stdin=none, sh='sh')`, `cmd \| exec()` | Runs one command and returns its stdout (at most one trailing newline stripped). A **string** runs under `sh -c`; a **list** is argv with no shell, each element one argument. More than one positional argument is an error. The child's environment is `env`. |
| `url(address, method='GET', headers={}, body=none, timeout='10s')` | The response body as text; a non-2xx status fails the Source. Daemon only. |
| `plugin(name, symbol, args...)` | Calls `const char* symbol(const char* input)` in `lib<name>` found on `[daemon.plugin] path`. Daemon only. |
| `path \| read` | The file's text (`~` is `$HOME`, a relative path is against the instance's directory), or none when it is missing. |
| `lup(name, all=false, files=false, dirs=false, follow=true, git=false, root=false)` | The nearest path called `name` at or above the instance's directory (stopping at `~`), or none: `root = "{{ lup('.git') \| dirname }}"`. |
| `text \| fromjson`, `text \| fromini` | Parse JSON, or INI (`section -> {key: string}`), into a value. |
| `a \| merge(b, ...)`, `merge(a, b, ...)`, `merge(list)` | Deep-merge objects; the last wins. |
| `obj \| map('filter', args...)` | Apply a filter to each value of an object, keeping its keys (a list maps as jinja's `map` does). |
| `value \| pluck('a.b[0]')` | Deep get; none on any miss. The path is a key's address, in the key grammar `comb get` reads: `[-1]` is the last element, `['a.b']` a name holding a dot, and a path `comb get` refuses (`[01]`) is an error. |
| `path \| dirname` | All but the last component; none stays none. |
| `value \| tojson` | Serialise, for `exec(..., stdin=...)`. |

`$BAR` inside an `exec` string is expanded by the shell from the child's environment, so the engine never sees it: a Source whose lines are hashed (a future `cache()`) cannot tell `$BAR` changed. Prefer `exec('cmd ' ~ env.bar)` (quote the value if it may hold spaces), so that what the command reads is written in the line.

**Instances.** `root` renders the directory an instance answers for. An empty `root` (or none) is the one instance at `~`. A read from a directory asks the nearest instance first; by default a key missing there is looked for at the parent of that root, and so on up to `~` (cascade, per leaf, the nearer winning). `| nearest` on `return` makes the nearest instance answer alone: a key it lacks is absent, and a directory with no root has no instance (see [Where an instance lives](/docs/concepts/providers#where-an-instance-lives)). `git`, `direnv`, `python` and `terraform` are nearest; `mise` and `asdf` cascade.

**Decay is data.** How many steps an entry decays through before it is evicted is a number in the `:cache` facet, not a fixed ladder. It ships as `[daemon.providers._.cache._] decay = 4`; set it for one provider with `[daemon.providers.<p>.cache._] decay = N`, or at runtime with `comb put '<p>:cache._.decay' N` (`'*:cache._.decay'` for all), 1 to 16.

### A provider's own lines — `template`, `result`, `return`

`[daemon.providers.<p>]` takes three jinja lines of its own. An absent one comes whole from `[daemon.providers._]`, the abstract provider the shipped `defaults/00-comb.toml` writes:

```toml
[daemon.providers._]
template = "{{ client.providers[_.name].template }}"     # this provider's client templates, seeded as the :template facet
result   = "{{ merge(sources) }}"                         # call each Source, merge the objects
return   = "{{ result | facet('template', template) }}"   # the data, with the :template facet beside it
```

- `template` runs when the facets are seeded, once per configuration layer, over that layer's `[client]` tree (`client`); `_.name` is the provider. What it gives is `<p>:template`, labelled with the layer's file. The default is the provider's own `[client.providers.<p>] template` table. It must give a table of value expressions (string leaves, no dotted or empty keys); the daemon never renders the leaves, but checks each one ([Template facets](#template-facets)).
- `result` runs on a read, for each directory the read reaches. `sources` is a map of callables over the provider's **own** Sources, in declaration order (a config Source's declaration order is its name order, since the file keeps none; a built-in's is the order its provider lists): `sources.head()` is that Source's stored record at the directory being read, or `none` when it has none (a call never runs the Source; the read has already brought it up to date). `merge(sources)` calls every Source in order and deep-merges the records, so `{{ merge(sources) }}` is the whole of "a provider is its Sources". With the default, two Sources of one provider may not declare the same field (a load error naming both); a `result` that names its Sources, `merge(sources.a(), sources.b())`, has chosen an order, and the last one wins. A provider with no Sources is a provider all the same: its `result` is read once, at the global entry, with `sources` empty — so a literal (`result = "{{ {'a': 1} }}"`) answers. With no `result` line of its own the default merges no Sources and gives nothing, so a read of it is a miss. Where no file defines the abstract provider at all (a daemon handed a configuration without the managed `defaults/` layer), `result` is `{{ merge(sources) }}` and there is no `template` or `return`, so no template half.
- `return` runs once per `get`, over the data read (`result`) and the `:template` facet at the requested path (`template`). What it gives is what the provider answers, and a field read addresses into it: `return = "{{ result | merge({'x': 1}) | facet('template', template) }}"` makes `comb get p.x` answer 1. `result | facet('template', template)` hands the data on and attaches the facet beside it; that attached tree is the `template` half of the answer. A `return` with no `facet('template', …)` carries no template half: `return = "{{ result }}"` makes `comb get demo.t` miss while `comb get demo:template.t` still reads the facet. Under any `result` or `return` other than the shipped ones a field read demands every Source, since the line, not a Source, says what exists: it warms every Source at every level of the directory chain, not just the one that declares the field. That holds for any difference, a cosmetic `| facet(...)` on a `return` included, and for overriding `_`'s `return` or `result`, which changes every provider at once. A `return` that fails to render, or gives something other than a map, makes the read a miss (logged once); the data is not served around it. A Source's `comb.<p>` sees what a `get` of `<p>` answers, `return` included.

A line a file does not set is the abstract provider's, whole. `| super` pipes a value into the line one step up, and means something on `return` alone: `return = "{{ result | facet('mine', 1) | super }}"` attaches `mine` and then does what the abstract `return` does. A `template` or `result` line is the whole value and takes nothing piped in, so `| super` there is a load error; write the whole line. `_.name` is the provider the line is being applied to.

```toml
# git's templates are aws's, wherever the client wrote them
[daemon.providers.git]
template = "{{ client.providers.aws.template }}"

# Two Sources that agree on a field: the second wins
[daemon.providers.demo]
result = "{{ merge(sources.first(), sources.second()) }}"
```

### Switching a provider off — `enabled = false`

```toml
# git produces nothing, and nothing of it runs
[daemon.providers.git]
enabled = false
```

`enabled = false` on `[daemon.providers.<p>]` sets the provider's `result` to `"{{ {} }}"` and its `return` to `"{{ result }}"`, and none of its Sources is selected, called or run. What follows from it:

- **A read is a miss.** `comb get <p>` and `comb get <p>.<field>` find no data and no template half, so a template-only provider (`aws`, `op`, `conda`, `gcloud`, `kubecontext`, `talos`) switches off the same way. The `<p>:template` facet is still seeded and readable by its own name (`comb get aws:template`), and records a `put` stores under `<p>` are still read, since `result` carries them.
- **No Source of the provider runs.** A read, a `watch` and the scheduler's demand find no Sources to act on, so none is executed, polled, watched or kept warm. `comb refresh <p>` is `nothing to refresh: <p> is disabled`, and `comb get --force <p>` is an error, as it is for a static namespace; `--wait` is a miss. This holds for built-in providers too (`[daemon.providers.git] enabled = false` stops git's polling and its file watches), and `comb check providers` marks the provider `disabled`.
- **A call to one of its Sources is a miss.** `daemon.providers.<p>.sources.<s>()` from another provider's line, or from a Source's `data`, gives `none` and runs nothing, so `merge(...)` skips it. It is not a load error: a later layer may set `enabled = true` again.
- **`enabled = false` wins over a `result` or `return` on the same provider.** Neither is read (both are still checked at load). Two meanings for one provider would be two ways to do one thing, and layered files make a conflict an error nobody can fix from where they are: a drop-in that disables a provider need not know what an earlier file wrote.
- **It is a bool, and a layer may take it back.** Layers replace the key like any other, so a later `enabled = true` re-enables the provider. A value that is not a boolean is an error, and so is `enabled` on `[daemon.providers._]`, which applies to every provider: name the provider. A table that holds only `enabled` for a name no built-in, Source or client template gives a provider is an error too (`no provider is named 'gti'`).

A provider line sees only its own names (each line, like a Source's, is checked for every name it reads, and every line that reads one it does not see is an error of its own): `template` sees `client` and `_`, `result` sees `sources`, `daemon` and `_`, `return` sees `result`, `template`, `daemon` and `_` (`result` is also the value piped in by a `| super`; a `return` reaches its provider's own records through `result`, which keeps the `put` and `config:` layers over the Sources). The lines run on every read and do no IO: `exec`, `read`, `lup`, `url` and `plugin` are load errors in them. Schedule decorators (`poll`, `watch`, `ttl`, `failback`, `budget`, `nearest`) belong to a Source's `return`; on a provider's line they are a load error. A `result` line that fails to render makes that layer of the provider's records a miss; a `return` line that fails makes the whole read a miss, data and template half both. Either is logged once per distinct failure (after 256 distinct failures one line says further ones are suppressed).

### Template facets

A **facet** is a namespace holding data *about* a provider, written `provider:facet`. The `template` facet holds the value expressions a `comb get` on that provider returns alongside the data. Declare one under `[client.providers.<p>]` as `template` — the table maps 1:1 onto the tree, nested tables are groups, and string leaves are the expressions (the provider's `template` line, above, is what joins it into the facet):

```toml
[client.providers.python]
template.venv_name = "{{ python.venv }}"

[client.providers.python.template.widgets]
version = "{{ python.version }}"
```

That is `python:template.venv_name` and `python:template.widgets.version`. A provider exists if either root names it: one named only under `[client.providers]` or `[daemon.providers]` is a provider with no Sources. The rules are checked file by file, because seeding is, and enforced at daemon startup, on reload and by `comb check config`; each error names its file:

- `[client]` holds only `providers`, and `[client.providers.<p>]` holds only `template`; any other key is `unknown key '<k>' in [client]` (or `[client.providers.<p>]`). `[daemon.providers.<p>]` holds `cache`, `sources`, `enabled` (a boolean; [not on `_`](#switching-a-provider-off--enabled--false)) and the three lines above, and nothing else.
- Nesting is tables only. A quoted key containing a `.` (`"a.b" = "…"`) is one name, not a path, and reads as the quoted segment `t['a.b']`; an empty key is an **error**. A provider name is the head of a key, so it holds no `:`, `.`, `[`, `]` or `*` and does not start with `/`, `~` or `<` — the same rule a `put`, a key and a Source name follow — and it is none of the reserved words `env`, `comb`, `_` and `*`, which mean something of their own in every key and template (`_` is the daemon root's prototype table, `[daemon.providers._]`).
- A template leaf that is neither a string nor a table is not a value expression: an **error**.
- Every template leaf is checked as the value expression it is, whatever wrote it to `<p>:template` (the provider's `template` line included), though the daemon never renders one. A leaf that does not compile is an **error**, `config(<file>): [client.providers.<p>] template.<leaf>: syntax error: …`, and nothing of that file's tree for `<p>` is seeded. A leaf that reads a name nothing supplies — not `env`, not `comb`, and no provider the daemon has or the configuration names — is a **warning**, `… template.<leaf>: unknown name 'gti' (this line sees env, comb, git, …)`: `comb check config` shows it as `WARN` and the daemon logs it, but it loads, because a `put` may yet make that provider. `comb put` takes a template on the same terms: one that does not compile is refused, and a name no provider has is taken.
- The colon-table form `[providers."<p>:template"]` is gone with the whole `[providers]` root: it is `unknown table [providers]`, which says templates live in `[client.providers.<p>] template.<leaf>` and decay in `[daemon.providers.<p>.cache._]`.

The decay depth is seeded the same way, from the daemon root: `[daemon.providers.git.cache._]` with `decay = 2` seeds `git:cache._.decay`, and `[daemon.providers._.cache._]` seeds `*:cache._.decay` for every provider (the shipped `defaults/00-comb.toml` sets 4). `cache` holds only `_`, `_` holds only `decay`, and `decay` is an integer from 1 to 16 (`comb put <p>:cache._.decay` takes the same range).

Every leaf is loaded into the cache at daemon start, labelled with the file it came from — `comb status` shows its source as `config(~/.config/beachcomber/conf.d/10-mine.toml)`. Every file's leaves are loaded under that file's own label, and a leaf two files set is read from the later file in composition order (`defaults/`, then `config.toml`, then `conf.d/`). `comb status --tree` lists the file that answers and, under it as `shadowed`, each file it overrode.

**An edit under `providers` reloads without a restart.** A configuration change that touches only what is under `[daemon.providers]` or `[client.providers]` — Sources, `template`, `result` and `return` lines, `cache` tables, `enabled`, a provider newly named or no longer named — is applied in place. The daemon builds the new set of providers from the composed files, swaps it in, and reseeds the `template` and `cache` facets: every `config:`-labelled record is dropped and all layers are loaded again, so an edited leaf takes effect on the next read, a leaf deleted from its file disappears, and a `watch` on the affected key wakes. A file whose records in a provider come back unchanged keeps their age, so `comb status -a` shows when each file last changed them rather than when the daemon last reloaded. Records written by `comb put` are untouched. Only a change to a bare key of `[daemon]` or `[client]` (`socket_path`, `log_level`, `plugin.path`, `timeout`, `budget`, the watchdog keys, `idle_shutdown`, …) restarts the daemon, because those are read once at startup.

What a reload does to what is already running, instance by instance:

- A Source that is no longer there, whose provider is now `enabled = false`, or whose body changed (its `root`, `data` and `result` lines, `_` prototypes included) is evicted: its records and stored `:cache` keys go, with its watches and the roots and failure state kept for it. A Source that still exists runs on the next read that asks for it, so an edited body takes effect at once rather than when its records next expire; a changed `root` evicts the instances of the old one.
- A Source whose schedule changed (`poll`, `watch`, `failback`, `ttl`, `budget`, `nearest`, or the decay depth that applies to it, so an edit to `cache._.decay` retunes every instance of the provider) is retuned in place: it keeps its records, its place in the lifecycle and its failure count, takes the new schedule, registers its new watches at once and restamps the schedule stored with its records (`<p>:cache`), without running. A failback change starts its failure count afresh.
- Every other Source keeps its records, its timers and its watches. Its failure count and backoff carry over.
- Every `root` is worked out afresh on its next read.
- The plugin libraries stay loaded for the daemon's life: a reload never closes one, and does not look at the files again.
- A request in flight finishes on the providers it started with, and so does a run the daemon has under way: it reads the Sources it calls through them. A run that a reload overtook (its Source was evicted or retuned) writes nothing and is run again under the new providers. A `watch` stream follows the reload: it works out again what it reads.
- Puts are untouched, and the namespace of a removed provider that still holds puts stays reachable as a static namespace.

The reload is all or nothing, twice over. The edited files are checked against the rules above, and the providers they declare are built, before anything is touched, so an edit that breaks one of them changes nothing at all — the previous values keep being served and the error is logged (and shown by `comb check config`) until the file is fixed. And each namespace's old and new records are swapped together, so a `comb get` running concurrently with the reload never sees a value momentarily missing.

### Templates and value expressions

A **template** is a string leaf of a `template` table: a value expression over `env.*`, `comb.*` and other fields' resolved values. The daemon stores it and serves it; the client — `comb get`, `comb eval`, and every SDK's `resolve`/`eval`, all through `libbeachcomber::resolve` — renders it with the caller's environment and directory. A field need not have a cached value to carry a template:

```toml
[client.providers.scanline]
template.theme = "{{ env.SCANLINE_THEME or 'dark' }}"

[client.providers.scanline.template.widgets]
foo = "{{ env.SCANLINE_FOO }}"
bar = "{{ comb.git.branch | truncate(12) }}"
```

**Addressing.** Every node is addressable at any depth, templated or cached alike: `comb g scanline.widgets.foo` renders one leaf, `comb g scanline.widgets` renders the group as an object, and `comb g scanline` returns the whole namespace — its cached fields deep-merged with its rendered template tree. Where a template sits at the same path as a cached field, the template wins on read (`defaults/terraform.toml` does this to `terraform.workspace`, reading the cached value back through `comb.terraform.workspace`). The raw template text is read through the facet: `comb g scanline:template.widgets.foo` prints `{{ env.SCANLINE_FOO }}`. An address can run past a template into what it renders — with `stats = "{{ {'cpu': 40} }}"`, `comb g scanline.stats.cpu` is `40`.

**Expressions.** One rule, everywhere a template is evaluated:

| Written as | Evaluates to | Example |
|---|---|---|
| exactly one `{{ expr }}` tag | the expression's natural type; `none` or undefined is absent | `"{{ env.OP_SERVICE_ACCOUNT_TOKEN \| bool }}"` → a bool |
| literal text and/or more than one tag | a string, `none` printing as nothing | `"{{ git.branch }}{% if git.dirty %}*{% endif %}"` → `main*` |
| no tags at all | its own literal text | `"env.HOME"` → the string `env.HOME` |

Inside an expression, `comb.<provider>.<field>` is the raw cached value; `<provider>.<field>` is that field as `comb get` would resolve it (templates included, recursively, cycle-detected); `env` is the caller's whole environment. An address is written the way `comb get` takes it, jinja's subscripts included: `comb.mise['cargo:x']`, `comb['git:cache'].branch`, `comb[name]` with a computed name, `env[var]`, `env | items`. A literal attribute chain is read exactly — `comb.git.branch` fetches `git.branch` alone, so only git's `head` Source runs — and a ref used any other way (subscripted, filtered, iterated) reads the node at the end of its literal chain whole: `comb.aws_profiles[name]` reads `aws_profiles`, `scanline['widgets']` resolves `scanline`. `comb` alone is every provider, so `comb[...]` reads the provider it names when the render asks for it (`comb['git']` reads all of git — write `comb.git.branch` for one field). What a render read, computed names included, is what a `watch` follows. A missing ref is undefined and falsy, and so is an unset `env.X` (`env.X | default('d')` gives `d`; `env.X | bool` is whether it is set and non-empty). A leaf whose single tag yields `none` or undefined (`"{{ env.MYAPP_ENV or comb.myappcache.workspace }}"` with neither set) is absent, never `""`: `comb get` of it is a miss and a group read omits it, as in the daemon's own lines. A number JSON cannot hold (`{{ 1 / 0 }}`) is a runtime error. Filters: minijinja's built-ins plus `truncate`, `basename`, `fg`, `bg`, `bold`, `dim` and `fromjson`; the engine's `merge`, `map` over objects, `pluck`, `dirname`, `fromini`, `read`, `lup` and `exec` render on the client too, while `url` and `plugin` exist only on the daemon.

**Errors.** A template that does not compile, or a cycle, is a configuration error: a single-leaf query fails, and a namespace query fails naming the leaf (`comb g scanline` exits 2 with `scanline.widgets.foo: …`). A runtime failure — a failed or disabled `exec()` — fails a single-leaf query but is omitted from a namespace object, as an absent leaf is. A group with no present leaf left is absent itself, at every level: `comb g kubecontext` with no context set is a miss, not `{}`, and `{{ kubecontext is defined }}` is false. An object stored as data — `{}` written by a `put` or a Source — is a value even when empty.

**Writing a template at runtime.** `put` addresses the facet like any other node: `comb p scanline:template.widgets.foo '"{{ env.OTHER }}"'` takes effect on the next read and lasts until the daemon restarts; it may also remove it with `null`, and `--global` writes `$HOME`. A `put` layers over a Source or `config:` value at the same directory — it replaces the node there and hides what is beneath it — and removing the put shows them again. Flags and semantics are under `comb p` in [CLI Commands](/docs/reference/cli-commands).

**Managed defaults.** The shipped templates — `aws`, `conda`, `gcloud`, `op`, `python`, `terraform`, `kubecontext`, `talos` — are files the daemon writes to `~/.config/beachcomber/defaults/` before it loads configuration, each headed by `# Managed by comb <version>` and rewritten on a version change (a shipped name with this version's marker is left untouched even if edited). `defaults/` is the lowest composition layer — `defaults/*.toml`, then `config.toml`, then `conf.d/*.toml` — so override one field by setting the same key in your own file and leaving the rest to the default:

```toml
# ~/.config/beachcomber/conf.d/10-mine.toml
[client.providers.python]
template.version = "{{ env.MY_PY or comb.python.venv_version }}"   # venv_name still comes from defaults/python.toml
```

`comb check config` reports the directory, its file count and version, and warns about files in it that `comb` did not write. A directory the daemon cannot write is a refusal to start.

**The 0.9 form is an error.** The `[providers]` root — where 0.9 kept a provider's templates (`[providers.<name>.virtual]`, `[providers."<name>:template"]`) and its knobs — is reported by daemon start and `comb check config` as `unknown table [providers]`, naming the file and saying where its contents went: templates in `[client.providers.<name>] template.<leaf>`, a provider's other settings in `[daemon.providers.<name>]`. A `path` key was the path-expression setting; a query addresses the caller's directory, `--path`, or `--global`.

### `exec()` in templates

A template can call `exec()` to run a command and use its output as the field's value — `defaults/kubecontext.toml` renders `kubecontext.context` this way:

```toml
[client.providers.myapp]
template.deploy_tag = "{{ exec('git describe --tags --always') }}"
```

| | |
|---|---|
| Signature | `exec(cmd, timeout='10s', stdin=none, sh='sh')`, or the filter `cmd \| exec()` |
| Spawn | a **string** runs under `sh -c` (`sh=` names another shell); a **list** is argv, no shell — each element exactly one argument, `'$FOO'` stays literal. |
| Environment | the child's environment is exactly the template's `env` map (`comb`'s environment for the CLI, `env_json` for an SDK), so `$FOO` and `env.FOO` agree; its working directory is the render's. |
| List element coercion | string → itself; bool → `"true"`/`"false"`; number → its canonical string form; `undefined`/`none`, a sequence, or a map → error (structured data goes through `stdin=` instead) |
| `timeout` | keyword, a duration (`'2s'`, `'500ms'`, or a number of seconds), default `'10s'` |
| `stdin` | keyword, default none; must be a string — `\| tojson` first for structured data |
| Output | stdout, lossy-UTF-8, at most one trailing newline stripped; capped at 1 MiB (an oversized stdout is a failure, not a truncation) |
| Failure | non-zero exit / timeout / oversized stdout / spawn failure → a runtime error naming the command, with truncated stderr attached |
| Availability | `comb get`, `comb eval`, and an SDK client constructed with `exec` on (`Client(exec=True)`, `new Client({ exec: true })`, `beachcomber.WithExec(true)`, `Beachcomber::Client.new(exec: true)`, `comb.connect{ exec = true }`, `bc_client_new("{\"exec\":true}")`) — off by default. Never the daemon, and never a formatting surface (`-f fmt`, `watch -f fmt`, the status formatter). A call anywhere it is off fails with `exec() is disabled in this context`. |

A whole-namespace `comb get <provider>` renders every template in that namespace in sequence, so several `exec()`-backed leaves under one namespace can serialise up to N × `timeout` in the worst case — pass an explicit, low `timeout` when the consumer is latency-sensitive (a shell prompt or statusline).

See [CLI Commands](/docs/reference/cli-commands) for the expression-level reference and more examples.

### Composition / conf.d

Config can be split across multiple files using the `conf.d` convention —
no `include =` directive needed. Any `*.toml` file placed in
`~/.config/beachcomber/conf.d/` is composed onto `config.toml` automatically:

```
~/.config/beachcomber/
├── defaults/            # written by the daemon — the shipped templates
│   ├── aws.toml
│   └── …
├── config.toml
└── conf.d/
    ├── 10-scanline.toml
    └── 20-overrides.toml
```

Composition rules:

- The managed `defaults/*.toml` files (see [Templates](#templates-and-value-expressions))
  are the lowest layer, in lexical order; `config.toml` and then `conf.d/`
  compose on top, so a key you set anywhere else overrides a default.
- Files in `conf.d/` are merged in **lexical filename order** (a numeric
  prefix like `10-`, `20-` is a common way to control that order).
- Each file is **deep-merged** onto the accumulated config: tables merge key
  by key (a drop-in overriding one key of `[daemon.providers.x]` doesn't clobber the
  rest of that block), while scalars and arrays are **last-wins** (the last
  file to set a given key or array wins outright — arrays are replaced, not
  concatenated).
- `config.toml` itself is optional; `conf.d/*.toml` files compose fine with
  no main file present.
- `comb check config --origins [KEY]` shows the result: every key, the file
  that set it, and each earlier file it overrode, marked `shadowed`; a setting
  no file sets shows `default` (see [CLI Commands](/docs/reference/cli-commands)).
- A file that is not valid TOML is an **error** naming the file, in
  `config.toml`, `conf.d/` or `defaults/`: the daemon refuses to start on it
  and `comb check config` fails. It is never skipped.
- Configuration is read only from `$XDG_CONFIG_HOME/beachcomber` (default
  `~/.config/beachcomber`): `defaults/`, `config.toml`, `conf.d/` and the
  `env` file beside them. `$XDG_CONFIG_DIRS` is not consulted.
- Config changes take effect **without a manual restart**. Changes to
  `config.toml`, any `conf.d/*.toml` or any `defaults/*.toml` file (including
  files added or removed) trigger the daemon's config self-watch, which acts
  only once the *whole* composed set parses cleanly and its providers load; an
  edit that doesn't is ignored with a logged error. An edit confined to
  `[daemon.providers]` or `[client.providers]` is [applied in place](#template-facets);
  a change to a bare key of `[daemon]` or `[client]` restarts the daemon
  automatically.

## Config field summary

**`[daemon]` section:**

| Field | Type | Default | Description |
|---|---|---|---|
| `socket_path` | string | (see resolution order) | Unix socket path. Resolution order: this key → `BEACHCOMBER_SOCKET` env var → `/tmp/beachcomber-<uid>/sock` |
| `log_level` | string | `"info"` | Tracing log level |
| `timeout` | duration, at least `1ms` | `"10s"` | How long one run of a Source may take before it is cut off, for every Source whose `return` does not name `timeout(...)` |
| `env_file` | string | `env` beside `config.toml` | Path to env file loaded at startup; `comb check config` reads it too, so it checks with the variables the daemon would have |
| `watchdog_interval` | duration or null | `null` (disabled) | How often the watchdog checks scheduler liveness |
| `watchdog_threshold` | duration or null | 3x `watchdog_interval` | Stale heartbeat duration before triggering shutdown |
| `idle_shutdown` | duration (whole seconds) or null | `null` (disabled) | How long with no client before the daemon shuts down; unset keeps it resident |
| `budget` | duration, at least `1ms` | `"50ms"` | How long a read waits for a Source before answering with what it has, for every Source whose `return` does not name `budget(...)`. Recorded, not yet enforced: a later release reads it. |
| `providers.<p>.enabled` | bool | `true` | `false` switches the provider off: sugar for `result = "{{ {} }}"`, and no Source of it runs ([Switching a provider off](#switching-a-provider-off--enabled--false)). Not valid on `_` |
| `providers.<p>.cache._.decay` | int, 1 to 16 | (4 from `defaults/00-comb.toml`) | Decay steps before eviction, seeded as `<p>:cache._.decay`; `<p> = _` is every provider ([Template facets](#template-facets)) |

**`[client]` section:**

| Field | Type | Default | Description |
|---|---|---|---|
| `providers.<p>.template.<leaf>` | string, or a table of them | none | A template (value expression), seeded as `<p>:template.<leaf>`; nest with tables ([Template facets](#template-facets)) |

> Values are checked when the configuration loads, and an error names the file: `log_level` is one of `off`, `error`, `warn`, `info`, `debug`, `trace`; `timeout` and `budget` are durations of at least `1ms`, and `idle_shutdown` whole seconds of at least `1s`; `socket_path` and `env_file` are not empty; `watchdog_interval` and `watchdog_threshold` are whole seconds of at least `1s` (`"30s"`, `"5m"`, `"1h"`, or a TOML integer of seconds), a threshold needs an interval, and a threshold below the interval is an error. Every duration takes the one grammar in [CLI Commands — Durations](/docs/reference/cli-commands#durations). `[daemon]` takes no other keys. A file that is not valid TOML is an error too, never skipped, and the error says where and why in one line: `config(<file>): TOML parse error at line 5, column 1: duplicate key `glyph``. Every configuration error takes that one form, `config(<file>): …`, naming the file that set what it is about and, where a table is at fault, the table as written (`config(~/.config/beachcomber/conf.d/10-mine.toml): [daemon.providers.demo.sources.s] data: unknown name 'nope' (…)`); the daemon's refusal, the reload log and `comb check config` show the same lines, and every mistake is reported at once — a mistyped setting hides no Source, and a broken Source no other. That one — a dotted key (`template.glyph = …` under `[client.providers.p]`) adding to a table a `[client.providers.p.template.…]` header already created — is the commonest: TOML forbids it, so write such keys under the `[client.providers.p.template]` header instead.

> A Source's schedule is its `return` line (`poll`, `watch`, `ttl`). The `[providers]` root, the `[lifecycle]` (`idle_shutdown_secs` became `[daemon] idle_shutdown`), `[failback]` and `[tree]` tables, and the `scope`, `marker`, `poll_*`, `fsevent_*`, `failback_*`, `budget_*` keys are config errors, at whatever level they appear, and so is `enabled` anywhere but `[daemon.providers.<p>]`. `[daemon] provider_timeout_secs`, `idle_shutdown_secs` and the `[daemon.budget]` table are errors that name their replacements: `timeout`, `idle_shutdown` and `budget`, each a duration.

**`[daemon.providers.<name>.sources.<source>]` section for a built-in Source:**

A table naming a built-in Rust Source takes `return` only; `root`, `data` and `result` are errors (`git.diff is a built-in Source: only return can be set`). The line replaces the Source's invalidation, failback, decay, budget and `nearest` whole; a failback, budget or timeout it does not name is the engine's (`failback(3, '1s')`, `[daemon] budget`, `[daemon] timeout`). `| super` extends the nearest `_` Source's line, as for a config Source. A built-in with no `return` keeps its own schedule: the `_` lines do not reach it.

**Calling another Source.** A Source's `data` line may call a sibling, `sources.<source>()`, or any provider's Source, `daemon.providers.<provider>.sources.<source>()`; a provider's `result` and `return` lines may call the latter, as in `return = "{{ result | merge(daemon.providers.gitstats.sources.repo()) | facet('template', template) }}"`. A call gives the called Source's own stored record, without any `put` over it, as a read of it from the same directory would find it: a cascading Source merges up its chain, a `| nearest` Source answers from its nearest instance alone. A Source whose record is not stored yet is run inline, once, by the read or refresh that needs it: two calls to it in one read, or callers racing for it in one refresh, run it once. A provider line's call is warmed by the `get` before the line renders; the same call reached from `comb.<p>`, a `watch` frame or `<p>:cache` finds only what is stored and never runs anything or renders a `root`. A called Source is kept warm while it is called: a read that reaches it through a line demands it, and demanding a Source demands what its last run called (a poll of the caller does not; a branch of the `data` line the last run did not take calls nothing), so it is polled and watched on its own schedule and decays and is evicted with its callers: one that would be evicted first stays at its last decay step while a caller that is still cached names it. `get --force` and `get --wait` reach the Sources a provider's `result` and `return` lines call as they reach its own, re-running them (or those past their interval). A `watch` of a provider follows the Sources its lines call. A leaf a line computes from a called record is as old as that record. The abstract provider `_` cannot call a Source by name (`sources.<s>()` is a load error there: use `merge(sources)`), since its lines apply to every provider. A called Source's own failure is the caller's miss, as a direct read of it would be (an instance of a cascading Source that fails is skipped, and the call goes on up its chain): the line that called it gets nothing, and the failure is the called Source's, under its own failback. A Source cannot call itself (`<p>.<s> cannot call itself`), a call to a provider or Source that does not exist is a load error naming it, and Sources that call each other in a ring are a load error naming the ring (`Sources call each other in a ring: a.x -> b.y -> a.x`). Were a ring to be run anyway, the Source reached again fails the read with `sources: <p>.<s> calls itself`.

**`[daemon.providers.<name>.sources.<source>]` section (config Sources):**

| Field | Type | Required | Description |
|---|---|---|---|
| `data` | string | yes | The body: a jinja line that runs on the daemon, usually `exec('command')`. Sees `env` (the daemon's own), `comb.<provider>` (also `comb['<provider>']` and a computed `comb[name]`, each a read of that provider's stored records), `sources.<source>()` (a sibling Source), `daemon.providers.<provider>.sources.<source>()` (any provider's Source) and `_`. |
| `result` | string, or table of strings | yes | `result = "{{ data }}"` owns the whole provider node; `result.<field> = "{{ … }}"` lines declare fields, each over `data`. |
| `root` | string | no | The directory an instance answers for, e.g. `{{ lup('.git') \| dirname }}`. Empty (the default) is the one instance at `~`; on a `\| nearest` Source, a `root` that renders none means no instance. |
| `return` | string | no | What the Source hands out, decorated: `poll(interval, count)`, `watch(path, …)`, `ttl(interval)`, `failback(count, interval)`, `budget(interval)`, `nearest` (the nearest instance answers alone instead of cascading up to `~`; see [Where an instance lives](/docs/concepts/providers#where-an-instance-lives)). Absent, it takes the nearest `_` Source's (`defaults/00-comb.toml` ships `poll('60s', 12) \| failback(3, '1s')`). `\| super` extends that line instead of replacing it. |

A table named `_` (`[daemon.providers.<name>.sources._]`, `[daemon.providers._.sources._]`) is a prototype: the defaults for every Source below it, never a Source itself. Prototypes apply to config Sources, and to a built-in Source's `return` only through `| super`: a built-in with no `return` of its own keeps its Rust schedule.

**`[daemon.plugin]` section:**

| Field | Type | Default | Description |
|---|---|---|---|
| `path` | array of strings | `["<config dir>/plugins"]` | Directories `plugin(name, symbol, args...)` searches, in order, for `lib<name>.dylib` (macOS) or `lib<name>.so` (Linux). Each is absolute or starts `~/`. A later file's list replaces an earlier one's. |
