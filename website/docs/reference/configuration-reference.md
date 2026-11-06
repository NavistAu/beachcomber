---
sidebar_position: 3
title: Configuration Reference
---

# Configuration Reference

Every configuration key, table by table. comb needs no configuration to run. [Configuration](../concepts/configuration.md)
explains how the pieces fit; [Custom Providers](../custom-providers/overview.md) shows how to use them.

Every duration below takes the one grammar in [CLI Commands: Durations](./cli-commands.md#durations) (`250ms`, `30s`,
`1m30s`, a bare number of seconds; in TOML an integer is seconds too). Every error names its file and table, in the form
`config(<file>): …`.

## Files and composition

| File | Written by | Layer |
|---|---|---|
| `~/.config/beachcomber/defaults/*.toml` | the daemon: the shipped providers, headed `# Managed by comb <version>` | lowest, in lexical order |
| `~/.config/beachcomber/config.toml` | you; optional | over `defaults/` |
| `~/.config/beachcomber/conf.d/*.toml` | you | over `config.toml`, in lexical order |
| `~/.config/beachcomber/env` | you | not TOML: the [env file](#daemon) |

`~/.config/beachcomber` is `$XDG_CONFIG_HOME/beachcomber`; `$XDG_CONFIG_DIRS` is not read.

- Each file is deep-merged onto the ones before it: tables merge key by key, and a scalar or array from a later file
  replaces the earlier one whole.
- A file that is not valid TOML is an error, never skipped: the daemon refuses to start on it and `comb check config`
  fails.
- The daemon rewrites a `defaults/` file when its version changes; set the key in your own file rather than editing it.
  A file in `defaults/` that comb did not write is a warning in `comb check config`. A `defaults/` directory the daemon
  cannot write is a refusal to start.
- `comb check config --origins [KEY]` shows each key, the file that set it, and each file it overrode (`shadowed`).

```
~/.config/beachcomber/
├── defaults/          # git.toml, battery.toml, aws.toml, 00-comb.toml, ...
├── config.toml
├── conf.d/
│   ├── 10-mine.toml
│   └── 20-overrides.toml
├── plugins/           # default [daemon.plugin] path
└── env
```

### Reloading

The daemon watches every file above. An edit takes effect once the whole composed set loads; one that does not is
ignored, logged, and shown by `comb check config`, and the previous configuration keeps serving.

| An edit to | Takes effect by |
|---|---|
| anything under `[daemon.providers]` or `[client.providers]` | reloading in place, within seconds; puts are untouched |
| a bare `[daemon]` key or `[daemon.plugin]` | restarting the daemon, automatically |
| the env file | `comb kill`; the next read starts a daemon that loads it |

On an in-place reload, a Source whose `root`, `data` or `result` changed is evicted and runs on its next read; one whose
schedule changed keeps its records and takes the new schedule; every other Source is untouched. Plugin libraries stay
loaded.

## Roots

A file has two top-level tables. Anything else at the top level is an error.

| Table | Holds |
|---|---|
| `[daemon]` | the daemon's settings, `[daemon.plugin]` and `[daemon.providers]` |
| `[client]` | `[client.providers]`: what clients render (templates, macros) |

`[providers]`, `[lifecycle]`, `[failback]` and `[tree]` are errors that say where their contents moved.

## `[daemon]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `socket_path` | path | `$BEACHCOMBER_SOCKET`, else `/tmp/beachcomber-<uid>/sock` | The Unix socket. The CLI's `--socket` beats it |
| `log_level` | `off` `error` `warn` `info` `debug` `trace` | `info` | Log level. Logs go to `$XDG_STATE_HOME/beachcomber/daemon.log` |
| `timeout` | duration, ≥ 1ms | `10s` | How long one run of a Source may take, for every Source whose `return` has no `timeout(...)` |
| `budget` | duration, ≥ 1ms | `50ms` | How long a read of an out-of-date record waits for its Source's run before answering with what is stored, for every Source whose `return` has no `budget(...)` |
| `env_file` | path | `env` beside `config.toml` | `KEY=VALUE` lines loaded into the daemon's environment at startup; blanks and `#` comments skipped, one pair of quotes stripped |
| `idle_shutdown` | duration, whole seconds, ≥ 1s | unset: stay resident | How long with no client before the daemon exits; the next client starts a new one |
| `watchdog_interval` | duration, whole seconds, ≥ 1s | unset: no watchdog | How often the watchdog checks the scheduler's heartbeat |
| `watchdog_threshold` | duration, whole seconds | 3 × `watchdog_interval` | How stale the heartbeat may get before the daemon exits for its supervisor to restart. Needs an interval and may not be below it |

A path may start `~/` (`~user` is an error) and may not be empty. Any other key is an error. Removed keys name their
replacement: `provider_timeout_secs` is `timeout`, `idle_shutdown_secs` is `idle_shutdown`, the `[daemon.budget]` table
is `budget`.

```toml
[daemon]
log_level = "debug"
timeout = "30s"
idle_shutdown = "30m"
env_file = "~/.secrets/comb.env"
```

## `[daemon.plugin]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `path` | list of directories, each absolute or `~/` | `["<config dir>/plugins"]` | Where `plugin(name, …)` looks for `lib<name>.dylib` or `lib<name>.so`, in order. A later file's list replaces the earlier one |

```toml
[daemon.plugin]
path = ["~/.local/lib/beachcomber", "/opt/beachcomber/plugins"]
```

See [Shared-library plugins](../custom-providers/plugins.md).

## `[daemon.providers.<p>]`

One table per provider. `<p>` names a provider, or is `_`, the **abstract provider** whose lines every provider without
its own takes.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `sources.<s>` | table | none | A Source ([below](#daemonproviderspsourcess)) |
| `enabled` | bool | `true` | `false` switches the provider off ([below](#enabled)). Not on `_` |
| `template` | jinja line | from `_` | Builds the provider's `:template` facet from `[client]` |
| `result` | jinja line | from `_` | What the provider holds at a directory, from its Sources |
| `return` | jinja line | from `_` | What a read of the provider answers |
| `cache._.decay` | integer, 1 to 16 | 4 (from `_`) | Backoff steps an unread entry takes before eviction. Seeded as `<p>:cache._.decay`; on `_`, every provider's |

Any other key is an error. A provider exists once either root names it, even with no Sources.

```toml
[daemon.providers.kv]
return = "{{ result | merge({'label': 'kv'}) | super }}"

[daemon.providers.kv.cache._]
decay = 2
```

### Provider lines

The shipped abstract provider, in `defaults/00-comb.toml`:

```toml
[daemon.providers._]
template = "{{ client.providers[_.name].template }}"
result   = "{{ merge(sources) }}"
return   = "{{ result | facet('template', template) }}"
```

| Line | Runs | Sees | Gives |
|---|---|---|---|
| `template` | when facets are seeded, once per file | `client` (that file's `[client]` tree), `_` | a table of template leaves: the `:template` facet |
| `result` | on a read, at each directory it reaches | `sources`, `daemon`, `_` | the provider's data there. `merge(sources)` merges every Source's record; `sources.<s>()` is one Source's |
| `return` | once per read | `result`, `template`, `daemon`, `_` | the answer. `\| facet('template', template)` attaches the template half |

- `_.name` is the provider the line applies to.
- An absent line is `_`'s, whole. `| super` on `return` pipes into `_`'s `return`; on `template` or `result` it is an
  error.
- The lines run on every read and do no IO: `exec`, `read`, `lup`, `url`, `plugin` and the schedule decorators are load
  errors in them.
- Under any `result` or `return` other than the shipped ones, a field read runs every Source of the provider.
- With the default `result`, two Sources of one provider may not declare the same field. A `result` that names them,
  `merge(sources.a(), sources.b())`, chooses the order, and the last wins.
- A line that fails to render makes the read a miss, logged once.
- `facet(name, value)` takes `template`, `macro`, `memo`, `put`, `source` or `cache`; only `template` is read off a
  `return`.

### `enabled`

```toml
[daemon.providers.battery]
enabled = false
```

| Effect | |
|---|---|
| reads | a miss: no data and no template half. `<p>:template` is still readable by name, and puts under `<p>` still read |
| Sources | none is run, polled, watched or kept warm, built-in ones included |
| `comb refresh <p>` | error `nothing to refresh: <p> is disabled` |
| `comb get --force <p>` | error |
| a call of one of its Sources from another line | `none`; not a load error |
| `comb check providers` | `disabled` |
| `result`, `return` on the same provider | not used (still checked at load) |

It is a bool, and a later file may set `true` again. A table holding only `enabled` for a name no provider has is an
error (`no provider is named 'gti'`).

## `[daemon.providers.<p>.sources.<s>]`

A Source. `<s>` is a name, or `_` for a **prototype**: `[daemon.providers.<p>.sources._]` gives defaults to every Source
of `<p>`, and `[daemon.providers._.sources._]` to every Source of every provider. A prototype is never a Source itself.

| Line | Type | Required | Sees | Meaning |
|---|---|---|---|---|
| `root` | jinja line | no | `env`, `_` | The directory an instance answers for, usually `{{ lup('<marker>') \| dirname }}`. Empty or none: one instance at `~`. Under `nearest`, none means no instance |
| `data` | jinja line | yes | `env`, `comb`, `sources`, `daemon`, `_`, and any provider by name | The body, run in the daemon with the root as working directory. Can call every engine function, `exec`, `url` and `plugin` included |
| `result` | jinja line, or a table of them | yes | `data`, `_` | `result = "{{ data }}"` makes the whole value the provider's node; `result.<field> = "…"` declares fields one by one |
| `return` | jinja line | no | `result`, `_` | The schedule: `result` piped through [decorators](#return-decorators) |

- `env` is the daemon's environment, env file included. `comb.<p>` reads another provider as a `get` would.
  `sources.<s>()` calls a sibling Source, `daemon.providers.<p>.sources.<s>()` any provider's; a Source cannot call
  itself.
- In `data`, a provider is also read by its plain name, `{{ git.branch }}`, and means the same as `comb.git.branch`:
  the value the daemon stores. The line's own words (`env`, `comb`, `sources`, `daemon`, `_`) win over a provider of
  the same name; `comb.<p>` still reads it.
- A name in `data` that no provider has, or that only a client template gives (the daemon never renders one), is a
  load **warning**, since a `put` may yet supply it. In the other lines, a name the line does not see is a load error,
  not a silent `none`.
- An absent line is the nearest prototype's. `| super` on `return` pipes into the next prototype's `return`; a
  decorator the nearer line sets wins.
- The shipped default is `[daemon.providers._.sources._] return = "{{ result | poll('60s', 12) | failback('1s', 3) }}"`.
- Any other key is an error.

The functions and filters `data` and `result` can call are in the [Template Library](./template-library.md); `url()`'s
arguments are in [From an HTTP API](../custom-providers/from-an-http-api.md#url-arguments). `cache()`, and `watch()` and
`ttl()` as filters, are client-template items and a load error here.

```toml
[daemon.providers.pkg.sources.main]
root = "{{ lup('package.json') | dirname }}"
data = "{{ 'package.json' | read | fromjson }}"
result.name = "{{ data.name }}"
result.version = "{{ data.version }}"
return = "{{ result | watch('package.json') | poll('5m') | nearest }}"
```

### `return` decorators

A `return` line must decorate `result`. Arguments are constants; a decorator takes no keyword arguments. A later call of
one decorator in a line replaces an earlier one.

| Decorator | Arguments | Default | Meaning |
|---|---|---|---|
| `poll(interval, count)` | interval: whole seconds, 1s to 1 year; count: 1 to 10000 | count 12 | Re-run every `interval` while read; after `count` runs unread, start decaying |
| `watch(path, …)` | one or more non-empty paths | | Re-run when a path changes. Relative to the root; `/`, `~` or `$` start an absolute path; `'.'` is the whole root; `'!dir'` leaves `dir` out of `'.'` (needs `'.'`) |
| `ttl(interval)` | as `poll`'s interval | | `poll(interval, 1)` with a decay of 1: the value expires. Not with `poll` |
| `failback(interval, count)` | interval first, as a string (`'1s'`); count 1 to 10000 | `'1s', 3` | After `count` failures in a row, retry from `interval`, doubling |
| `budget(d)` | duration ≥ 1ms; a number is whole seconds | `[daemon] budget` | How long a read waits for this Source's run |
| `timeout(d)` | duration ≥ 1ms | `[daemon] timeout` | How long one run may take |
| `nearest` | none | off | The nearest instance answers alone; no fallback up the directory chain |
| `super` | none | | Continue with the next prototype's `return` |

`watch` and `poll` together re-run on either. A Source with neither runs on its first read and keeps that value until
`comb refresh` or eviction. A decorator on a provider's own line is a load error.

```toml
return = '''{{ result
  | watch('.', '!.git')
  | poll('30s', 4)
  | failback('60s', 3)
  | budget('100ms')
  | nearest }}'''
```

## `[client.providers.<p>]`

What clients render. The daemon stores these and never evaluates them.

| Key | Type | Meaning |
|---|---|---|
| `template.<leaf>` | string, or a table of them | A template, seeded as `<p>:template.<leaf>`. Nested tables are groups |
| `macro.<name>` | string | A [macro library](./macros.md), seeded as `<p>:macro.<name>` |

Any other key in `[client]` or `[client.providers.<p>]` is an error.

```toml
[client.providers.prompt.template]
project = "{{ pkg.name ~ '@' ~ pkg.version if pkg.version else none }}"

[client.providers.prompt.template.widgets]
mode = "{{ env.THEME_MODE or 'dark' }}"
```

### Template leaves

| Written as | Evaluates to |
|---|---|
| exactly one `{{ expr }}` | the expression's own type; `none` or undefined is absent |
| statements, then one `{{ expr }}`, only whitespace between | the expression's own type |
| text and/or more than one tag | a string |
| no tags | its literal text |

- A leaf that is not a string or a table is an error. A leaf that does not compile is an error, and nothing of that
  file's tree for `<p>` is seeded.
- A leaf that reads a name nothing supplies is a warning, not an error: a `put` may create that provider later.
- A template at the same path as a cached field wins on read.
- A later file replaces one leaf, or one macro library, and keeps its siblings.

The language is in [Templates](../concepts/templates.md) and the [Template Library](./template-library.md).

## Names

A provider name, a Source name, a template leaf and a macro name follow one rule:

- no `:`, `.`, `[`, `]` or `*`, and not starting with `/`, `~` or `<`;
- not empty; a quoted key holding a `.` (`"a.b" = …`) is one name, read as `t['a.b']`;
- not `env`, `comb`, `_` or `*` as a provider name.

A dotted key (`template.glyph = …` under `[client.providers.p]`) cannot add to a table a
`[client.providers.p.template.…]` header already created; TOML forbids it. Write such keys under the
`[client.providers.p.template]` header instead.

## Errors

The daemon at startup, a reload and `comb check config` check the same rules and report every mistake at once, each
naming its file and table:

```
  [FAIL] local    config(~/.config/beachcomber/conf.d/96-bad.toml): [daemon.providers.p.sources.s] return: failback(): takes the interval first, as a string like '1s', then the count: failback('1s', 3)
  [FAIL] local    config(~/.config/beachcomber/conf.d/96-bad.toml): [daemon.providers.q.sources.s] result.x: cache(): a client template's filter; a Source's records are the daemon's
  [FAIL] local    config(~/.config/beachcomber/conf.d/96-bad.toml): unknown key 'other' in [client.providers.r]
  [FAIL] local    config(~/.config/beachcomber/conf.d/96-bad.toml): unknown key 'bogus' in [daemon]
  [FAIL] local    config(~/.config/beachcomber/conf.d/96-bad.toml): [daemon] budget must be a duration of at least 1ms ("50ms", "2s"), got 0
  [WARN] local    config(~/.config/beachcomber/conf.d/96-bad.toml): [client.providers.r] template.ok: unknown name 'gti' (this line sees env, comb, …)
```

A `FAIL` stops the daemon starting, or a reload applying. A `WARN` loads.
