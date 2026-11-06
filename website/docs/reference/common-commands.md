---
sidebar_position: 2
title: Command Tour
---

# Command Tour

A tour of the commands you will use day to day, each with a real run and its output. [Getting Started](../concepts/values-and-names.md)
covers your first query; this page goes further. Every flag is listed in the
[CLI reference](../reference/cli-commands.md).

The examples run in a git repository at `~/proj`, on a branch `feature/login` with uncommitted changes.

## Read a value: `comb get`

`comb get <key>` prints one value. A key is `namespace.field`: `git` is the namespace, `branch` the field.
Every command has a one-letter short form, so `comb g` is `comb get`.

```shell-session
$ comb g git.branch
feature/login
```

A read of a value nothing has computed yet runs whatever provides it and waits for the answer, so the first read of a
key can take a moment; later reads come from the cache.

### A whole namespace

Leave off the field to read every field in the namespace, one `field=value` line each:

```shell-session
$ comb g hostname
name=laptop.local
short=laptop
```

Several keys in one call print one line each, in order:

```shell-session
$ comb g git.branch git.dirty battery.percent
feature/login
true
80
```

A key nothing answers prints nothing and exits `0`. Add `--strict` to make that exit `1`, for scripts that need to
tell "empty" from "missing".

### Which directory

Some namespaces answer for a directory (`git` answers for the repository around it); others are the same everywhere
(`hostname`, `battery`). A read addresses the current directory unless you say otherwise:

| You write | It reads |
|---|---|
| `comb g git.branch` | the current directory |
| `comb g git.branch .` | the current directory, spelled out |
| `comb g git.branch ~/proj` | another directory: a last argument starting with `/`, `~` or `.` |
| `comb g git.branch -p ~/proj` | the same, as a flag |
| `comb g myapp.status --global` | the global entry, `~`, ignoring where you are |

```shell-session
$ cd ~
$ comb g git.branch
$ comb g git.branch ~/proj
feature/login
```

The first read prints nothing: `~` is not in a repository. Values cascade from a directory up to `~`; see
[The Cascade](../concepts/cascade.md).

### Output formats

Text is the default, made for `$(…)` in a shell. `-f` picks another format, and a suffix on the command is its short
spelling: `comb g.j` is `comb g -f json`.

```shell-session
$ comb g -f json git.ahead
1
$ comb g.j git.ahead git.branch hostname.short
[
  1,
  "feature/login",
  "laptop"
]
$ comb g.s git.branch git.dirty
branch='feature/login'
dirty='true'
```

`json` keeps types (a number stays a number) and prints `null` for a miss. `sh` writes quoted assignments you can
`eval "$(comb g.s git)"`. There are also `csv`/`CSV` and `tsv`/`TSV` (uppercase adds a header row):

```shell-session
$ comb g.T hostname
name	short
laptop.local	laptop
```

### A template

Anything passed to `-f` that holds `{{` or `{%` is a template. Reading a namespace binds its fields by name:

```shell-session
$ comb g -f '{{ branch }}{% if dirty %}*{% endif %}' git
feature/login*
$ comb g.f '{{ branch }} +{{ ahead }}' git
feature/login +1
```

The template language is explained in [Templates](../concepts/templates.md).

## See what is cached: `comb status`

`comb status` (`comb s`) shows the cache as a table, one row per field, holding the values a read from this directory
would answer. This excerpt is from the repository, after a prompt had read `git`, `hostname` and `aws`:

```shell-session
$ comb s
  PROVIDER  FIELD    SOURCE          VALUE          PATH     AGE   POLL  TTL
  aws       profile  rendered        ▸ default      ~/proj     -      -  ---
  git       ahead    source(status)  1              ~/proj   13s×0  45s  ★  60s×02 (2m) ∙
  git       branch   source(head)    feature/login  ~/proj    14s      -  ★              ∙
  hostname  short    source(host)    laptop         ~        13s×0 9m46s  ★ 600s×06 (1h)
```

| Column | Meaning |
|---|---|
| PROVIDER, FIELD | The key: `git` and `branch` are `git.branch` |
| SOURCE | What produced the value: a provider's Source (`source(head)`), `put` for stored data, or `rendered` for a template your shell rendered just now (marked `▸`) |
| PATH | The directory the value belongs to; `~` for a global value |
| AGE | Time since the value was computed; `×0` is how many polls have run since anyone last read it |
| POLL | Time until the next poll |
| TTL | `★` while the value is being read; a number counts down the idle steps left before eviction. `60s×02` is the poll rate and count, `(2m)` the time to eviction, `∙` that it watches files |

`comb status --help` prints the same legend. Filter with `--filter key=value` (repeatable; `!=` negates):

```shell-session
$ comb s --filter provider=git --filter field=branch
  PROVIDER  FIELD   SOURCE        VALUE          PATH     AGE  POLL  TTL
  git       branch  source(head)  feature/login  ~/proj   25s     -  ★           ∙
$ comb s --filter value=main
  PROVIDER  FIELD           SOURCE            VALUE  PATH     AGE  POLL  TTL
  git       branch_default  source(branches)  main   ~/proj   24s     -  ★           ∙
  git       branch_parent   source(branches)  main   ~/proj   24s     -  ★           ∙
```

`-a` shows every row, including facets and absent values; `--tree` shows the layers behind each value; `-f json` or
`-f tsv` give machine-readable rows. See [`comb s`](../reference/cli-commands.md#comb-s-status).

## Follow a value: `comb watch`

`comb watch` (`comb w`) prints a value now, then a new line each time it changes, until you press Ctrl-C:

```shell-session
$ comb w git.branch
feature/login
fix/typo
```

The second line appeared when another shell ran `git switch -c fix/typo`. It takes the same formats as `get`;
`comb w.j` prints one compact JSON document per change.

## Combine values: `comb eval`

`comb eval` (`comb e`) renders a template over any keys, all fetched in one request:

```shell-session
$ comb e '{{ git.branch }}{% if git.dirty %}*{% endif %} on {{ hostname.short }}'
feature/login* on laptop
$ comb e '{{ env.USER }} up {{ uptime.days }}d'
jane up 52d
```

A template that is a single `{{ … }}` keeps its value's type, which `-f json` shows:

```shell-session
$ comb e '{{ git.ahead + git.behind }}'
1
$ comb e.j '{{ git.dirty }}'
true
```

## Store your own values: `comb put`

`comb put` (`comb p`) stores JSON under a key, so a script can publish state for your prompt or status bar to read.
It prints nothing on success.

```shell-session
$ comb p deploy '{"status":"passing","build":412}'
$ comb g deploy
build=412
status=passing
```

A put applies to its directory and every directory below it, and `--global` stores one value for everywhere:

```shell-session
$ cd ~/proj/src
$ comb g deploy.status
passing
```

A put replaces what it addresses. Put a field to change only that field; put the namespace to replace all of it:

```shell-session
$ comb p deploy.status '"failing"'
$ comb g deploy
build=412
status=failing
```

`null` removes a value:

```shell-session
$ comb p deploy null
$ comb g.j deploy
null
```

`--ttl` makes a value expire: it lives for the ttl, lingers for one idle step, and is then removed. Putting it again
renews it:

```shell-session
$ comb p --global build.eta '"4m"' --ttl 30s
$ comb g build.eta
4m
```

See [`comb p`](../reference/cli-commands.md#comb-p-put-key-json-data-path---ttl-duration--p-path---global).

## Get a fresh value: `refresh`, `--force`, `--wait`

Providers keep themselves current by watching files and polling, so you rarely need these. They are for when you know
something changed that the provider cannot see.

| Command | Use it when |
|---|---|
| `comb r git` | A hook or script should recompute a value, and needs no answer. `comb r git.commit` reruns only the Source that owns `commit` |
| `comb g --force git.lines_added` | You want the recomputed value back now |
| `comb g --wait forever git.commit` | You want a read to wait for a run in progress, however long it takes |
| `comb g --wait 0 git.commit` | You want whatever is stored, at once, even if stale |

```shell-session
$ comb r git
$ comb g --force git.lines_added
1
```

`refresh` prints nothing on success. A namespace you made with `put` has nothing to recompute:

```shell-session
$ comb r deploy
error: deploy: nothing to refresh: deploy has no sources
```

## Check health: `comb check`

`comb check` (`comb c`) runs health checks and prints `[PASS]`, `[WARN]` or `[FAIL]` for each. With no subcommand it
runs all of them; name one to narrow it:

```shell-session
$ comb c daemon
Daemon
  [PASS] beachcomber 0.9.1 — pid 42062 — uptime 1m37s
  [PASS] socket   /tmp/beachcomber-501/sock
  [INFO] config   no config.toml
  [PASS] requests_total=308  in_flight=0  active_watchers=0  cache_entries=29
  [PASS] daemon responsive
  [PASS] in_flight=0
  [PASS] watch backend: native fs events
  [PASS] reaper: not armed (side daemon)
```

`comb c config` validates your configuration, and `comb c config --origins <key>` shows which file set a setting:

```shell-session
$ comb c config --origins daemon.providers.git.sources.diff.return
  KEY                                       SOURCE                                           VALUE
  daemon.providers.git.sources.diff.return  config(~/.config/beachcomber/defaults/git.toml)  {{ result | watch('.', '!.git') | poll('30s', 4) | failback('60s', 3) | budget('100ms') | nearest }}
```

It exits `0` when healthy, `1` on a warning and `2` on a failure. The other subjects (`providers`, `cache`, `watches`,
`timers`, …) are listed under [`comb c`](../reference/cli-commands.md#comb-c-check-subcommand).

## Stop the daemon: `comb kill`

The daemon starts on the first `comb` command and stops itself when idle. `comb kill` (`comb k`) stops it now, for
instance after upgrading comb; the next command starts a fresh one.

```shell-session
$ comb k
Daemon stopped (pid 42062).
$ comb k
Daemon is not running.
```

## Next

- [Built-in Providers](../providers/built-in-providers.md): every namespace and field that ships.
- [The Tree](../concepts/values-and-names.md): namespaces, facets, and cached against computed values.
- [CLI reference](../reference/cli-commands.md): every command and flag.
