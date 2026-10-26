---
sidebar_position: 1
title: CLI Commands
---

# CLI Commands

All commands are subcommands of `comb`. The daemon is socket-activated — you never need to start it manually.

Every command has a single-letter shorthand. Long forms (`get`, `status`, etc.) also work.

**Default output is plain text** — `comb g git.branch .` prints just the branch name. `-f` picks another format, or takes a template, and a suffix on the command is its short spelling:

| Suffix | `-f` value | Format |
|--------|------------|--------|
| _(none)_ | `text` | **text** (`get`, `watch`) or **human** (`status`): the defaults |
| `.j` | `json` | json — the bare value, nested at any depth |
| `.s` | `sh` | shell assignments, sourceable with `eval`/`source` |
| `.c` / `.C` | `csv` / `CSV` | comma-separated; uppercase adds a header row |
| `.t` / `.T` | `tsv` / `TSV` | tab-separated; uppercase adds a header row |
| `.f TEMPLATE` | `TEMPLATE` | a template: any `-f` value holding `{{` or `{%` |

A `-f` template renders exactly as `comb eval` renders one — `env`, `comb.*`, provider refs and `exec()` all read as they do there — with what the command read bound beside them: `value` (`values`, in key order, for several keys) and an object's fields by name on `get` and `watch`, and each row's fields (`provider`, `path`, `field`, `source`, `value`, `age_ms`, `age_human`, `stale`, `kind`, …) on `status`. A name it binds is never fetched as a provider. A template that does not compile fails the command once (`-f: template compile error: …`, exit 2); a `status` row whose render fails shows the error in its place.

A template is recognised by its tags, never by not being a format's name: `-f` with anything else must name a format the command has, or it's refused (`get has no 'bogus' format: expected text (default), json, sh, csv, CSV, tsv, TSV, or a template ({{ … }})`). `status` also has `table` (an uncoloured `human`), and `check config --origins` takes `human` or `json`.

| Short | Long | Formats |
|-------|------|---------|
| `comb g` | `comb get` | `text` (default), `json`, `sh`, `csv`, `CSV`, `tsv`, `TSV`, a template |
| `comb s` | `comb status` | `human` (default), `table`, `json`, `sh`, `csv`, `CSV`, `tsv`, `TSV`, a template |
| `comb p` | `comb put` | — |
| `comb r` | `comb refresh` | — |
| `comb w` | `comb watch` | as `get` |
| `comb e` | `comb eval` | — |
| `comb i` | `comb init` | — |
| `comb c` | `comb check` | `check config --origins`: `human` (default), `json` |
| `comb d` | `comb daemon` | — |
| `comb k` | `comb kill` | — |

A suffix applies only to a command that has `-f` and that format: `comb p.j` is `comb p takes no format`, and an unknown suffix is `unknown format suffix '.x'`.

## Addressing a directory

`get`, `put`, `refresh`, `watch` and `eval` read or write at a directory, and all five take it the same three ways:

- a trailing `[path]` argument, which is a directory only when it starts with `/`, `~` or `.` — no key starts so. For `get`, which takes any number of keys, a last argument written otherwise is another key, and a lone argument is always a key. For the others, whose last argument can only be a directory, anything else is refused with the spelling that works: `comb put p.k 1 repo` is `repo is not a directory: write ./repo or --path repo`;
- `-p <path>` / `--path <path>`;
- `--global`, the global entry (`$HOME`), which every directory's chain ends at.

The three conflict with one another, and with none the command addresses the current directory. A relative path is the caller's, not the daemon's, and a leading `~` the shell left unexpanded (because it was quoted) is `$HOME`; `~user` is an error. The SDKs read a path the same way, through the same function, and the daemon refuses any path that is not absolute.

A template rendered for a read runs its `exec()`, `lup()` and `read` in the directory the read addresses — the given path, else the current directory, and `$HOME` for `--global` — so `comb get t.x ./repo`, `comb eval '{{ t.x }}' ./repo`, `comb watch t.x ./repo` and an SDK's `resolve(t.x, cwd=./repo)` look at the same files.

## Durations

Every duration comb takes — `put --ttl`, `kill --timeout`, `check --connect-timeout`, `check procs --duration`, the `[daemon]` duration keys and the `poll`, `ttl`, `failback` and `budget` decorators — is written the same way: whole numbers each with a unit, `d`, `h`, `m`, `s` or `ms`, largest first, a bare number being seconds (`250ms`, `30s`, `1m30s`, `2h14m`, `2d3h`, `60`; in config a TOML integer is seconds too). Every duration comb shows is written this way too, so it can be copied back. Anything else is refused in the same words, before a daemon is started. What each one may be is its own: a ttl is whole seconds of at least `1s`, a poll interval at least `1s`, a `budget` at least `1ms`, a `--connect-timeout` of `0` is a single attempt.

Every duration comb shows — `status`'s AGE and POLL, `check`'s uptime, intervals and elapsed times, a timeout in an error message — reads one way: `14s`; `3m`, `1m18s`; `2h`, `2h14m`, the seconds dropped once there are hours; and `250ms` when under a minute and not whole seconds. The TTL column's poll-interval field (`60s×12`) stays whole seconds, to line its digits up.

## Exit codes

Every command exits the same way, whatever its format — the format is applied last and never decides the code:

- `0` — done. A read that found nothing, a miss, is done too: `text` prints nothing, `json` prints `null`, and a miss among several keys holds its place.
- `1` — a miss, under `--strict` (`get` and `eval`); and `check` finding something worth a look (`WARN`).
- `2` — an error: the daemon unreachable or refusing, a key the grammar cannot read, a template that does not compile, a failed `exec()`, a `kill` the daemon outlived, a `check` `FAIL`.

`--strict` is a global flag, accepted before or after the command (`comb --strict g git.branch`, `comb g git.branch --strict`); `BEACHCOMBER_STRICT=1` sets it (`0`, `false`, `no`, `off` or empty leave it off), and the flag wins.

## Colour and glyphs

`--color auto|always|never` and `--ascii` are global flags, accepted before or after the command, and every command that draws — `status`, `check`, `check config --origins`, `status --help`'s legend — draws by them. `--color` wins over `NO_COLOR`, which counts only when it holds something (an empty `NO_COLOR` is unset). Under `auto`, the default, a table is coloured on a terminal unless `NO_COLOR` is set. `--ascii` swaps every Unicode mark, tree branch, ellipsis and dash for its ASCII form.

The template filters `fg`, `bg`, `bold` and `dim` follow the same choice without asking for a terminal — a prompt reads them through a pipe — so they draw their escapes unless `NO_COLOR` is set in the render's environment or `--color never` is given, and `--color always` draws them whatever `NO_COLOR` says. `--color` is written into the environment of the run, so `env.NO_COLOR` and an `exec()` child see the same choice. An SDK's `resolve`/`eval` reads `NO_COLOR` from the `env` it is handed.

`--no-exec` is a global flag too: every template the command renders (`get`, `eval`, `watch`, a `status` row or `-f` template) runs with `exec()` off, so a template that calls it fails with `exec() is disabled in this context` and nothing is spawned. Without it, `comb` allows `exec()`, as it is the user's own shell. The SDK subprocess tiers pass it for a client whose `exec` option is off, which is the default.

## `comb g` (get) `<key>... [path] [-p <path>] [--global] [-f format]`

Query a value. Returns cached data immediately. On a cold cache (first query for a key), executes the provider inline and blocks briefly while it runs — subsequent queries return the cached value with no delay. `comb get` is a client of `libbeachcomber::resolve`, the same function behind `comb eval` and every SDK's `resolve`: it always needs the daemon, even for a key (`env.HOME`) it answers without a request.

**Flags:** `--force` (trigger immediate recomputation before returning: a Source re-executes, and a cached template renders past its memo; on a namespace a `put` made, an address with no template has nothing to force and is an error), `--wait <DURATION|forever>` (how long to wait for a run the read needs before answering what is stored: `--wait 0` answers at once, and takes a cached template's memo past its ttl rather than render it; `--wait forever` waits for the run; without it a stored record waits its Source's `budget` and a cold read waits for its run; the bare `--wait` is a usage error), `-p`/`--path <dir>` (query a directory other than the current one), `--global` (query the global entry, `$HOME`) — see [Addressing a directory](#addressing-a-directory). Multiple keys can be passed in a single connection (variadic).

**Path:** after at least one key, a last argument that starts with `/`, `~` or `.` is the directory to read from (`comb g git.branch .`, `comb g git "$PWD"`); no key starts with those, so a key holding a `/` (`mise.npm:@scope/pkg`) is always a key, and a lone argument is always a key. A leading `~` the shell left unexpanded (it was quoted) is `$HOME`. With no `--path` and no such argument, `get` queries the current working directory. A read at a directory resolves data stored by `put` (and by config) up the directory chain to `$HOME`, the nearest write winning; a provider's own data stays where its source scope puts it. `--global` reads `$HOME` alone and cannot be combined with a path.

**Key addressing forms:**

| Form | What it selects |
|---|---|
| `provider.field` | Single field — most common form |
| `provider.path.to.node` | Any node at any depth — a leaf returns a scalar, an interior node its subtree as an object |
| `provider` | The whole namespace: every cached field, deep-merged with every rendered template |
| `provider:template.path…` | The raw template text at that node, unrendered |
| `env.NAME` | The caller's own environment variable |

A key is jinja's own address syntax, so a key reads the same here as in a template. Each segment below the provider is one of:

| Spelling | Example | Notes |
|---|---|---|
| `.seg` | `git.branch`, `mise.cargo:cargo-nextest`, `mise.npm:@scope/pkg` | Any characters but `.`, `[` and `]` — a colon below the provider is just a character |
| `[n]` | `p.list[0]`, `p.list[-1]` | An array index; the same as `p.list.0`. A negative index counts from the end, as in jinja |
| `['…']` / `["…"]` | `'mise["go:golang.org/x/tools/gopls"]'` | A quoted segment, for a name holding `.`, `[` or `]`; a backslash escapes only the quote and the backslash |

Brackets are the one quoting form; quote the whole key for the shell (`comb g 'p["a.b"]'`). Every address comb prints — `comb g` text lines, the `comb s` FIELD column, error messages — is in canonical form and can be typed back as a key: a segment that needs quoting prints as `['…']`, as in `['go:golang.org/x/tools/gopls']=0.16.0`.

The `provider.field` form automatically routes to the source that owns the field — a source name is metadata on the record, never a key you address directly. Unlike a field, a source has no address at all.

**Templates.** A key whose namespace carries a template (`[client.providers.<p>] template.*` in configuration, the managed `defaults/`, or a `comb p <p>:template.<f>`) is rendered by `comb get` with this shell's environment and directory — `comb g python.version`, `comb g aws.region` and `comb g kubecontext.context` are all templates, and `exec()` is enabled here. A template at a cached field's path wins on read. A template that does not compile, or cycles, exits `2` with a message naming the field — for a namespace query too, so `comb g scanline` fails on a broken `scanline.widgets.foo` rather than silently omitting it; a runtime failure such as a failed `exec()` is omitted from a namespace object and fails a single-leaf query. See [Configuration Reference — Templates](/docs/reference/configuration-reference#templates-and-value-expressions). A template that caches its value (`| cache()`) is served from its memo while the memo is live, and runs nothing then; a read that renders it prints the value first and writes the new memo back before it exits. See [Caching a template](/docs/reference/template-library#caching-a-template).

```sh
# Query a specific field — text is the default
comb g git.branch .              # → main
comb get git.branch .            # equivalent long form

# Query a field from a global provider (no path needed)
comb g battery.percent           # → 87
comb g hostname.short            # → myhost

# Query the entire provider (all fields from all sources) — alternate formats
comb g git .                     # → key=value lines, one per leaf at any depth (text default)
comb g.j git .                   # → the bare JSON object, nested (no envelope — metadata is in the :cache facet)
comb g.s git .                   # → a shell assignment per leaf, every value shell-quoted
comb g.c git .                   # → one comma-separated row, a cell per field
comb g.C git .                   # → the same, under a header row of the field names
comb g.t git .                   # → one tab-separated row
comb g.T git .                   # → the same, under a header row

# Template formatting
comb g.f '{{ branch }} ({{ dirty }})' git .      # → main (false)

# Shell assignments, sourceable: `eval "$(comb g.s git .)"`
comb g.s git .
# ahead='0'
# behind='0'
# branch='main'
# dirty='false'
comb g.s git.branch .            # → branch='main' (a scalar is named by its key's last segment)

# Multiple keys in one connection (variadic)
comb g git.branch git.dirty battery.percent .

# Force immediate recomputation before returning
comb g --force git .

# Wait for the fresh value however long its run takes (useful after a trigger)
comb g --wait forever git.branch .

# Answer at once with what is stored, however stale; the run goes on
comb g --wait 0 git.branch .

# Template-backed keys render here, with this shell's environment
comb g python.version .              # → 3.12.1 (defaults/python.toml's cascade)
comb g kubecontext.context           # → prod-cluster (kubectl, through exec())
comb g scanline:template.widgets.foo # → {{ env.SCANLINE_FOO }} (the raw template)

# The global entry ($HOME) alone, ignoring the current directory
comb g --global myapp.status

# Field metadata — the :cache facet mirrors the tree
comb g.j git:cache.branch .      # → {"age_ms": 1523, "stale": false, "source": "source(head)", ...}
comb g.j git:source .            # → what the Source computed, even where a put hides it
```

**Output.** Every format is rendered once, for `get`, `watch` and `eval` alike:
- **Absent.** A miss, or a `null` anywhere in a value, prints nothing — no line, no assignment — an empty cell where a column holds its place, and `null` only in json.
- **text.** A scalar prints bare. A container prints sorted `address=value` lines, one per leaf at any depth, each address canonical so it can be typed back as a key (`['a.b']=1`, `list.0=a`).
- **sh.** Every line assigns a shell variable: a leaf is named by its address below the key with each character outside `[A-Za-z0-9_]` written as `_` (`c.d` → `c_d`, `['a.b']` → `a_b`, `e f` → `e_f`), a scalar by the key's last segment, and every value is single-quoted, so `eval "$(comb g.s …)"` is safe.
- **csv and tsv.** A cell per top-level field of an object, or per key; a string is its raw text and anything else its JSON text. csv quotes per RFC 4180, tsv writes `\`, tab, CR and LF as `\\`, `\t`, `\r` and `\n`, and `CSV`/`TSV` add a header row of the cells' names.
- **Newlines.** A scalar printed under text has no trailing newline, so it embeds verbatim. Everything else — json, sh, csv, tsv, and text of a container or of several keys — ends each line in one.
- **Several keys.** One document across the keys, a miss holding its place: `null` in the json array, an empty line under text and sh, an empty cell under csv and tsv.

**Layers and the `:cache` facet.** A field's value is the first layer that has it: the effective template (rendered here), then `put` (what `comb p` and the `config:` seeds stored), then the Source. Nothing merges implicitly, `null` means absent, and each layer is readable on its own at any depth: `provider:template`, `provider:put` and `provider:source` (the last two are read-only), `provider:memo` (what a cached template stored; only its write-back writes it), and `provider:cache`, the daemon's metadata about the same tree.

| `:cache` key | Type | Description |
|--------|------|-------------|
| `age_ms` | int | Milliseconds since the value was last computed (0 for a rendered template) |
| `stale` | bool | Whether the value is past its expected refresh time |
| `source` | string | What answered: `put`, `template`, `memo` (a cached template's memo), `config(<file>)`, `computed(<line>)`, or `source(<name>)` such as `source(head)` |
| `hash` | string | The validity hash, 16 hex digits: a Source record's inputs as its run stamped them, or the hash a memo was stored under, until an invalidation drops it |
| `poll` | object | `{interval, count}` — present for polled Sources, `--ttl` puts and memos with a `ttl()` |
| `watch` | array | Watched paths — present for watching Sources and memos with a `watch()` |
| `decay` | int | Decay steps before eviction (default 4) |

Only a colon in the provider's own segment names a facet (`git:cache`), and the facets are `template`, `macro`, `memo`, `put`, `source` and `cache` alone: any other (`git:notes`) is `invalid facet key: git:notes (a facet is template, macro, memo, put, source or cache)`, exit `2`, on `get`, `put`, `refresh` and `watch` alike. Below the provider's segment a colon is an ordinary character, so `git.branch:age` is a miss like any unknown field. A key the grammar cannot read — `p..a`, `p['a`, `p[01]`, a provider starting with `/` or `~` — is `invalid key: <key>`, exit `2`.

**Exit codes** ([as for every command](#exit-codes)):
- `0` — success, a miss included in every format (e.g. `git` queried outside a git repository): `text` prints nothing, `json` prints `null`, and a miss among several keys holds its place
- `1` — a miss under `--strict`
- `2` — error (daemon unreachable, invalid key, a template that does not compile or cycles — the message names the field)

## `comb s` (status)

Show the warm cache as a table — one row per provider field, the values a read would answer. Facet rows (`aws:template`, `*:cache`) and absent values are hidden unless asked for (see [Filters](#filters)). SOURCE names what produced each row's value, and the `TTL` column encodes each entry's lifecycle position (`★` active, `3`/`2`/`1`/`0` decay countdown), effective poll interval, Active-window poll count, and whether the entry watches its files while it is Active (`∙`).

For a live view, ask for colour explicitly — under `watch` and `viddy` stdout is a pipe, so `--color=auto` (the default) resolves to no colour:

```sh
watch -c 'comb status --color=always'
viddy 'comb status --color=always'
```

`watch` needs its own `-c` to interpret the colour it is handed; without it the escape sequences render as literal text. Each refresh re-renders the caller's template leaves, `exec()` included, so a tight interval multiplies whatever the templates spawn.

```sh
$ comb s
PROVIDER  FIELD    SOURCE          VALUE    PATH    AGE   POLL  TTL
git       branch   source(head)    main     /repo   14s    46s  ★     60s×12 ∙
git       dirty    source(status)  true     /repo   14s    46s  ★     60s×12 ∙
hostname  short    source(host)    artemis  -        4m  5m46s  ★    600s×06

# Every row, facets and absent values included
comb s -a

# Filter to entries in a specific lifecycle state
comb s --filter=lifecycle=active
comb s --filter=lifecycle=decay1

# Sort options
comb s --sort age             # oldest first
comb s --sort stale           # stale entries first

# Script-friendly formats (bypass the human preset)
comb s -f tsv
comb s -f json
```

The machine formats lay a row out in the columns `PROVIDER PATH FIELD SOURCE VALUE AGE_MS STALE KIND DECAY POLL_INTERVAL_SECS POLL_COUNT FAILURE_CONSECUTIVE_FAILURES FAILURE_SUPPRESSED_UNTIL_UNIX_MS`, through the same cell encoders as `comb get`: `csv` and `tsv` have no header row and `CSV` and `TSV` do, a VALUE is its raw text (JSON for an object or array), and an absent field is an empty cell. `json` is one row object per line. `sh` names each variable `provider_path_field` with every character outside `[A-Za-z0-9_]` written as `_`, plus `_SOURCE`, `_KIND` and the lifecycle fields beside it.

**Flags:** `-f`/`--format` (`human` (default), `table`, `json`, `sh`, `csv`, `CSV`, `tsv`, `TSV`, or a template), `--filter <key=value|key!=value>` (repeatable; see [Filters](#filters)), `-a`/`--all` (drop the default filters), `--tree` (the layers behind each value), `--sort <default|provider|path|field|value|age|stale|lifecycle|poll_interval>` (`default` keeps each provider's rows together — its plain rows, then each facet's — by field, then by path with the global entry first), `--no-trunc`, `--max-width=auto|N` (default 120), `--depth N`, and the global `--color` and `--ascii` ([Colour and glyphs](#colour-and-glyphs)).

A path at or below `$HOME` is written from `~` in both tables, the global entry as `~` itself — in its symlinked form or its real one — and so is a file in a SOURCE label, `check`'s path lines and `check config --origins`. The machine formats keep the stored path, the global entry as an empty cell (`null` in json).

### Filters

`--filter` takes `key=value` or `key!=value`, repeatable; a row is shown when it matches every filter. `*` is a glob in any value, and `-` means none for `facet` and `value`.

| Key | Matches |
|-----|---------|
| `provider` | the provider's base name — `aws` matches `aws` and `aws:template` |
| `facet` | the facet after the colon (`template`, `cache`); `*` any facet; `-` none |
| `path` | the row's directory, as the table shows it or as stored: `~` is the global entry and `~/ws/*` a glob under home, as `/Users/me/ws/*` is |
| `field` | the field, exactly (destructured names included) |
| `source` | the SOURCE label shown |
| `value` | the value shown; `-` absent. A glob matches present values only, so `value=*` is "not absent" |
| `shadowed` | `true` / `false` — a record the read from here does not take: another layer of the same leaf answers (a nearer put, a template over data), or the read answers nothing there (a put replaced the node above it) |
| `stale` | `true` / `false` |
| `lifecycle` | `active`, `decay<N>`, `once`, `static`, `rendered` |

Three filters apply by default, so the table is what `comb get` here answers: `facet=-` hides facet rows, which are data *about* a provider rather than values a read answers; `value!=-` hides absent values; and `shadowed=false` hides records the read from this directory does not take. A filter you give on any of those keys, with either operator, replaces that key's default; `-a`/`--all` drops all three. Records stored at directories off this one's chain are other directories' values, and are listed as they are.

```sh
comb s                              # values only
comb s -a                           # everything
comb s --filter facet=template      # the template records
comb s --filter facet='*'           # every facet row
comb s --filter value=-             # only absent values
comb s --filter shadowed=true       # only what something else here shadows
comb s --filter provider!=mise      # everything but mise
```

An absent value is a template leaf that rendered nothing — a Source stores no absent field. Its row shows `null` in VALUE (dimmed with colour on) and `"value": null` in `json`.

### SOURCE, and the caller's rendered template leaves

SOURCE sits between FIELD and VALUE and names what produced the row, as a `:cache` answer does: a Source of the provider (`source(head)`, `source(refs)`, …), the static label `put`, a config-seeded record's `config(<file>)`, a leaf a provider line computed (`computed(return)`), or `rendered` for a row this CLI rendered itself, from the caller's own directory and environment, and `memo` for one it served from the leaf's memo (`| cache()`), whose AGE is the memo's; such a row ran nothing. A `<p>:memo` facet row (`--filter facet=memo`) is a memo as the daemon stores it, at its own directory. The function form keeps a Source named `put` or `rendered` from reading as one of those words.

The daemon holds a template (`[client.providers.<p>] template.*`, the managed `defaults/`, or a `comb p <p>:template.<f>`) as data and never evaluates it, so `status` used to show a column of raw expressions — `{{ env.SCANLINE_FOO }}` — where the point of looking is to see what a prompt would actually get. Every template leaf visible from the caller's directory (its own path, an ancestor up to `$HOME`, or the global entry) is now resolved exactly as `comb get` would resolve it and added as its own row, beside any cached value at the same address — and beside the raw template row, which `-a` shows:

```sh
$ comb p scanline:template.greeting '"hi {{ env.USER }}"'
$ USER=jane comb s -a --filter=provider=scanline
PROVIDER            FIELD     SOURCE    VALUE               PATH          AGE  POLL  TTL
scanline:template   greeting  put       hi {{ env.USER }}   ~/ws/project  0s      -  ---
scanline            greeting  template  hi jane             ~/ws/project  0s      -  ---
```

A cached field with a template over it shows **both** rows at that same address — `git.branch` cached by the `head` Source plus a `git:template.branch` override both list, so the raw cached value and the effective rendered one are visible side by side rather than one hiding the other. A leaf whose expression fails to render shows the failure text as its value instead of dropping the row.

A rendered row is marked in its VALUE cell: cyan when colour is on, a `▸ ` lead-in (`> ` under `--ascii`) when it's off. In the structured formats (`json`, `tsv`, `csv`, `sh`, a `-f` template's row fields) it carries `kind: rendered` and `source: rendered` (`memo` when served from its memo), and it's selectable on its own with `--filter lifecycle=rendered`. A `provider` filter that rules a provider out entirely skips resolving that provider's templates — and any `exec()` inside them — rather than rendering them only to discard the rows afterward.

Rendering resolves with this shell's own environment and directory, and `exec()` is enabled — `status` is one of the surfaces canon `field_resolution.md` invariant 15 names, alongside `comb get`, `comb eval` and `comb watch`.

In the `human`/`table` preset, SOURCE keeps its tail past 24 characters (less at a narrow `--max-width`, since it shares the same truncation budget as VALUE and PATH) — a `config(<file>)` label is a whole path, and its tail (`…/defaults/aws.toml`) is what tells one config file from another — unless `--no-trunc`.

### `--tree`: the layers behind each value

`--tree` follows each value row with the layers a read of it consults, in the order the read consults them: the template records first (`:template`, nearest directory first), then for each directory nearest first its `put`, its `config(<file>)` seeds and the Source records at the instances the read reaches. Every config file that sets a leaf is a layer of its own, the last in composition order (`defaults/`, then `config.toml`, then `conf.d/`) first, so a file is followed by those it overrides. The layers hang under the field name; the one the read takes is the row itself and is not repeated, and every other says what it is to the value — `:template` for a template record, `shadowed` for a layer the read did not take, dimmed as well when colour is on. A value with nothing behind it has no layers. A row on your directory chain (this directory, an ancestor, the global entry) is read from here; a row stored anywhere else is read from its own directory.

```sh
$ comb s --tree --filter provider=aws --filter field=profile --color never
PROVIDER  FIELD                  SOURCE                          VALUE                      PATH          AGE  POLL  TTL
aws       profile                rendered                        ▸ work                     ~/ws/project    -     -  ---
          ├─ :template           config(…/conf.d/10-mine.toml)   {{ env.MY_PROFILE … }}     -             20m
          ├─ :template shadowed  config(…/config.toml)           {{ 'personal' }}           -              2d
          └─ :template shadowed  config(…/defaults/aws.toml)     {{ env.AWS_PROFILE or … }} -              2d
```

A layer row's AGE is its record's own: how long since that file or `put` last changed it. Facet rows are not value rows and get no layers (`-a` still lists them). `--tree` destructures every value to its leaves unless `--depth` says otherwise, and works with the `human`, `table` and `json` formats — in `json` each row gains a `layers` array (`{label, facet?, path, value, age_ms, stale, answered?}`); any other format is an error. The layers come from the daemon's `layers` op ([protocol spec](https://github.com/NavistAu/beachcomber/blob/main/docs/protocol-spec.md)), which warms nothing.

**Destructuring (`--depth`):** object values expand into one row per key, recursively, with the parent's path, age and lifecycle metadata carried down. `--depth 0` is unlimited, `1` shows top-level fields only, `2` goes one level down, and so on.

| Preset | Default depth |
|--------|---------------|
| `human`, `table` | `2` — one level down |
| `json`, `tsv`, `csv`, `sh`, custom templates | `1` — no destructuring |

The human default stops at 2 deliberately: a provider caching a deep API response expands to thousands of rows at unlimited depth, and the long dotted field names then crowd out the VALUE column. Pass `--depth 0` when you do want the whole tree. The machine presets stay at `1` so scripts reading `-f json` keep the output they have today unless they ask for more. Arrays are never expanded.

```sh
$ comb p agent '{"session":{"name":"claude","ctx_pct":42}}'
$ comb s --filter=provider=agent
PROVIDER  FIELD            SOURCE  VALUE   PATH  AGE  POLL  TTL
agent     session.ctx_pct  put     42      -     0s      -  ---
agent     session.name     put     claude  -     0s      -  ---

$ comb s --filter=provider=agent --depth 1
PROVIDER  FIELD    SOURCE  VALUE                           PATH  AGE  POLL  TTL
agent     session  put     {"ctx_pct":42,"name":"claude"}  -     0s      -  ---
```

`--filter` and `--sort` see the destructured field names, so `--filter=field=session.name` works on the expanded rows.

**TTL column key:**

| Lead | Lifecycle | Meaning |
|------|-----------|---------|
| `★`  | Active    | alive, polling at base rate |
| `3`  | Decay 1   | 3 decay steps before eviction (at the default depth of 4) |
| `2`  | Decay 2   | 2 remaining |
| `1`  | Decay 3   | 1 remaining |
| `0`  | Decay 4   | 0 remaining — next tick evicts |
| `---` | Once / Static / Template | no lifecycle (hostname, put entries, a rendered template row) |

A `∙` trailing indicator means the entry watches its files while it is Active; a decaying entry has dropped its watches, and a read checks its files instead. A `⚠` lead replaces `★` when the provider is in failure-suppress (row renders red).

The default output preset is `human` (coloured table) regardless of whether stdout is a TTY. Scripts that want tab-separated data should pass `-f tsv` explicitly. Colour follows `--color`: `auto` (the default) colours only a TTY, so piping into a pager or a live viewer needs `--color=always`.

Use `comb check daemon` for daemon health (pid, uptime, version, active watchers, request counts, watch backend). The watch backend line reports `native fs events` when the startup self-test confirmed kernel event delivery, or a WARN with `polling` when it didn't — provider file-watching still works, invalidating on a 1s scan instead of kernel events.

## `comb p` (put) `<key> <json-data> [path] [--ttl <duration>] [-p <path>] [--global]`

Write static data into the cache. External processes use this to expose state to prompt/statusline consumers without writing a Source.

```sh
# Put application status into cache — scoped to the current directory
comb p myapp '{"status":"healthy","version":"1.2.3"}'

# Put with TTL — valid 30s, stale for one decay step, then removed; the next put renews it
comb p myapp '{"status":"healthy"}' --ttl 30s

# Put with path scope
comb p myapp '{"status":"building"}' --path /home/user/project

# Put once for every directory
comb p myapp '{"status":"healthy"}' --global

# Remove the node (null means absent; there is no other spelling of removal)
comb p myapp null

# Write a template — the next `comb g myapp.greeting` renders it
comb p myapp:template.greeting '"hello {{ env.USER }}"'
```

A template is checked as the configuration's are: one that does not compile is refused, naming the leaf (`myapp:template.greeting: syntax error: …`), and nothing is written. One that reads a provider nothing has yet is taken, since a later put may make it.

**Replace:** a write replaces whatever a put left at the node, so `comb p myapp '{"version":"1.2.4"}'` states all of `myapp` and a `status` written earlier is gone, while `comb p myapp.version '"1.2.4"'` replaces only that node. A `put` never removes a record a provider's Source produces: it sits above it in the layer order and hides it until the put is removed (`comb g provider:source` still shows the Source's answer). `comb p myapp null` removes the node and writes nothing in its place.

`data` at a namespace root must be a JSON object (`put data at the root must be an object`); below the root any JSON is legal, so `comb p myapp.deploy.count 3` and `comb p myapp.status '"healthy"'` are both fine. `data` is required: `comb p myapp` is a usage error, and removal is the JSON value `null`. A nested `null` inside `data` is an absent leaf, not `""`. Arrays are stored and returned as objects keyed by index.

**TTL:** `--ttl` (at least `1s`; not with `null`) makes the put a lifecycle entry of its own, one per put address: `comb p --ttl 300 build.status '"passing"'` and `comb p --ttl 60 build.eta '"4m"'` expire independently, and a re-put without `--ttl` drops the expiry. `comb g build:cache.status` shows its `poll` and `decay`.

**Decay depth:** the one `:cache` key a put may write is a decay prototype — `comb p '*:cache._.decay' 3` for every provider, `comb p 'git:cache._.decay' 6` for one, `--path` to scope either to a directory and below. The nearest prototype wins.

**Path:** `put` defaults to the current working directory, exactly as `get` does, so a plain `comb p` followed by a plain `comb g` in the same directory round-trips. `--global` writes `$HOME`, the entry every directory's chain ends at, which is how you publish one value for the whole machine; it cannot be combined with a path ([Addressing a directory](#addressing-a-directory)).

### Session-scoped entries

The `--path` value is a cache coordinate, not a filesystem lookup — a path that doesn't exist on disk is stored
verbatim. Appending a session discriminator to the directory gives each writer its own cache entry, so several agents
working in the same directory can publish independent state:

```sh
# Two agents in one project, each with its own path
comb p agent '{"context_pct":42}' --path "/home/user/project#session-a"
comb p agent '{"context_pct":97}' --path "/home/user/project#session-b"

comb g agent.context_pct "/home/user/project#session-a"   # 42
```

Reads must pass the exact path the writer used: `project#session-a` is a sibling name, not a child of `project`,
so a session-scoped path inherits from the parent directory (`/home/user`) and up — never from the plain project
directory. A relative path is resolved against your directory, as everywhere.

Read back with standard `comb g`:

```sh
comb g myapp.status        # healthy
```

There is no ownership: storing to a field name a Source already produces is allowed. The put is a layer of its own above the Source's, and removing it shows the Source's value again at once.

**Exit codes:** `0` on success, `2` on error.

## `comb r` (refresh) `<key> [path] [-p <path>] [--global]`

Re-execute the Sources a key demands, each at the instance that answers for the directory, without reading anything back. A bare `provider` re-runs every Source of the namespace; `provider.field` re-runs the one Source that owns the field — and only that one, so `comb r git.commit` runs `refs` and leaves `diff` alone. A Source with no root runs at the one instance at `~` whatever path the request names; a rooted one at the root that answers for the directory given.

```sh
comb r git                  # every git Source, for the current directory
comb r git.commit .         # the one Source owning `commit`
comb r git --path ~/project # a directory other than the current one
comb r hostname --global    # at the global entry ($HOME)
```

The path defaults to the current working directory, as `get` and `put` do, and a relative path is the caller's. `--global` cannot be combined with a path.

A key with no Source behind it — a `put`-created namespace, an unknown name, a field no Source owns — has nothing to re-execute, and saying so would claim work that never happened: it is an error, `nothing to refresh: <key> has no sources`. A refresh also drops the hash of every memo at or under the key, at every directory, so the next read of a cached template renders it afresh; a key with no Source but a memo there is then a refresh, and succeeds. Reading configuration afresh is not a refresh; the config watcher does that.

`refresh` prints nothing on success. It is the re-execution alone: `comb g --force` selects the same Sources but then *reads*, returning the value — the read-and-refresh form. Use `refresh` to warm or invalidate a key on a trigger (a git hook, a deploy step) and `get --force` when you also want the answer now.

**Exit codes:** `0` on success, `2` on error (the daemon's refusal, or an unreachable daemon).

## `comb w` (watch) `<key> [path] [-p <path>] [--global] [-f format]`

Stream a key's *resolved* value — the value `comb get` would print — printing a new frame each time that value changes. `comb watch` is a client of `libbeachcomber::watch`, the same fan-in behind every SDK's `watch`: it re-resolves the key over every reference the resolution reads. A write to a namespace or to any of its facets wakes that namespace's watchers, so a template edited under the watch fires it just as a changed cache record does. A watch may name any address, including one nothing answers yet (an unknown provider is a miss, as for `get`). Only an actual change in the resolved value emits a frame — a re-resolution that comes out the same value is not one. A cached template's memo losing its hash (a `refresh`, an edit of the template, a data write under the field) wakes the watch too: it renders the leaf again, writes the memo back before it prints, and prints only if the value changed.

```sh
comb w git.branch --path /home/user/project   # one line per resolved change
comb w git --path /home/user/project          # whole-provider, re-resolved on any change
comb w.j git.branch                           # bare JSON value per line, one compact document per frame
```

The path defaults to the current working directory, as `get`/`put`/`refresh` do, and a relative path is the caller's. `--global` cannot be combined with a path.

The first line printed is the value as it is when the watch opens — not a wait for the first change, the same answer `comb get` would give right now. Every later line is one resolved change. A miss is a frame as `comb get` prints one: `null` under `-f json`, the template's own rendering under a `-f` template (the one rendering `get` uses, with no value bound), and no line at all under every other format. Either way the watch stays open: a later `put` that gives the key a value prints it the moment it lands, and a value that goes away (`put <key> null`) is a `null` frame under json.

Frames render per format exactly as `comb get` renders that same value, with one difference: under `-f json` a frame is the bare value compacted to one line, where `comb get -f json` pretty-prints. `comb watch env.X` is an error before any connection is made — exit `2`, `env keys cannot be watched` — since an environment value is constant for the process and there is nothing to subscribe to. A facet key (`p:template`) is already inside a facet, so it has no template half to resolve: its frames are the daemon's own raw stream, unrendered, exactly as `comb get p:template -f json` prints the raw tree.

Ctrl-C to stop.

**Exit codes:** `0` — the watch ends cleanly once nothing is left that could ever answer (every subscription the daemon held has closed). `2` — error (daemon unreachable, an env key, a key whose resolution fails).

## `comb e` (eval) `<template> [path] [-p <path>] [--global] [-f <format>]`

Evaluate a value expression against cached provider data, resolving every referenced key in a single connection. `comb eval` renders through the same `libbeachcomber::resolve` as `comb get`, so `comb e '{{ p.f }}'` and `comb g p.f` agree by construction — and like `get` it always uses the daemon, even for an expression with no daemon refs.

`-f` takes the value formats `get` prints in: `text` (the default), `json` or `sh` (`e.j` and `e.s` spell the last two). `-f json` keeps a number a number and prints `null` for an absent value, which is how an SDK's subprocess tier reads an `eval`. Any other format is refused.

A value expression is written in one of three forms:

- **Single tag** — exactly one `{{ expr }}` spanning the whole argument apart from surrounding whitespace (`'{{ git.dirty }} '` is still a single tag): keeps the expression's natural type (bool, number, object, …). A `none` or undefined value is absent: `comb e` prints nothing, and `bc_eval` and every SDK's `eval` give null, as for a miss.
- **Statements, then one tag** — statements and comments, then the only `{{ expr }}`, with nothing but whitespace between (`'{% set x = 1 %}{{ x }}'`): the statements run and the expression keeps its natural type, so this is the number `1`. A macro's, a call's or a block `set`'s body does not count, since it prints nothing where it stands. The statements print nothing: one that does (a `{% call %}`) is `a typed line's statements print nothing`.
- **Template** — literal text and/or more than one tag: always string-valued. A source with no tags at all is a template too — it renders to its own literal text.

```sh
# Template (two tags + literal text) — a formatted prompt segment, string-valued
comb e "{{ git.branch }} | {{ battery.percent }}%" .

# Single tag keeping its natural type
comb e '{{ git.dirty }}' .

# No tags at all — literal text, rendered as written
comb e hello   # → hello

# With path
comb e "{{ git.branch }}" /home/user/project

# Template: conditionals and filters
comb e '{% if git.dirty %}*{{ git.branch }}{% else %}{{ git.branch }}{% endif %}' .
comb e '{{ git.branch | truncate(20) }}' .
```

Available filters: minijinja's built-ins (`default`, `upper`, `lower`, `length`, `tojson`, …) plus `truncate`, `basename`, `fg`, `bg`, `bold`, `dim` and `fromjson` (parse a string — typically `exec()` output — as JSON). `fg`, `bg`, `bold` and `dim` hand their text back unchanged under `NO_COLOR` or `--color never` ([Colour and glyphs](#colour-and-glyphs)).

The natural type a typed expression keeps is mostly *not* visible
in `comb e`'s own output, which is text: a bool prints `true`, and so does the
string `"true"`. Where it does show is in the shapes text cannot flatten — an
object prints as sorted `key=value` lines and an array as JSON — and in every
programmatic consumer: `bc_eval`, and so every SDK's `eval`, hands back a typed
value (bool, number, object, array). A template is always a string in all of
them.

**Exit codes:** `0` on success, an absent value included (it prints nothing); `1` for an absent value under `--strict`; `2` on error.

### `exec()` — running a command from an expression

A value expression evaluated by `comb e` can call `exec()` to run a command and use its output:

```sh
comb e '{{ exec("git rev-parse --short HEAD") }}' .
comb e '{{ exec(["git", "rev-parse", "--short", "HEAD"]) }}' .
```

| | |
|---|---|
| Signature | `exec(cmd, timeout='10s', stdin=none, sh='sh')`, or the filter `cmd \| exec()` (a command from both the pipe and an argument is an error) |
| Spawn | a **string** runs under `sh -c` (`sh=` names another shell): the shell expands `$FOO`, globs and pipes, and quoting is yours. A **list** is argv, no shell: each element is exactly one argument and `'$FOO'` stays literal. |
| Environment and directory | the child's environment is exactly the render's `env` map (this shell's environment for `comb`, `env_json` for an SDK) — `$FOO` and `env.FOO` read the same value — and its working directory is the render's. |
| List element coercion | string → itself; bool → `"true"`/`"false"` (not minijinja's `Display`, which prints `True`/`False`); number → its canonical string form; `undefined`/`none` → error (typically a missed daemon ref reaching `exec()` un-guarded); a sequence or map → error, pointing at `stdin=` instead |
| `timeout` | keyword, a duration in the one grammar (`'2s'`, `'500ms'`, or a number of seconds), default `'10s'`. A command still running at the deadline is killed (whole process group) and the call fails. |
| `stdin` | keyword, default none. Must be a string — pass structured data through `\| tojson` first (e.g. `stdin=(some_map \| tojson)`). |
| Output | stdout, lossy-UTF-8 decoded, with at most one trailing `\n` (and a preceding `\r`) stripped |
| Output cap | stdout past 1 MiB is a failure, not a truncation |
| Failure | a non-zero exit, a timeout, an oversized stdout, or a spawn error is an eval error naming the command, with stderr (truncated) attached |
| Availability | Every CLI surface that renders a template — `comb get`, `comb eval`, `comb watch`, `comb status`, and a `-f` template on any of them — since the CLI user already has a shell, and an SDK client constructed with `exec` on (`Client(exec=True)` and the equivalent in each SDK; off by default). A call on an SDK client without the option fails immediately with `exec() is disabled in this context`. |

`exec()` always runs client-side — the daemon never executes it, the same as every other value expression. See the [Configuration Reference](/docs/reference/configuration-reference#exec-in-templates) for a template that wraps a script call.

## `comb i` (init)

Detect installed tools (starship, powerlevel10k, tmux, neovim, polybar, waybar, sketchybar, oh-my-zsh, etc.) and print integration snippets for them.

```sh
comb i
```

Takes no subcommand. The output is appropriate shell/config fragments that you can source or paste into your shell RC, tmux config, neovim config, etc.

## `comb c` (check) `[subcommand]`

Run health checks and introspect daemon internals. Without a subcommand, runs top-level aggregation across all subjects.

```sh
comb c                   # aggregate across all subjects
comb c daemon            # daemon health: pid, version, uptime, request counts, watchers
comb c providers         # provider health and failure-suppress state
comb c config            # validate config file syntax
comb c config --origins  # every config key, the file that set it, what it overrode
comb c cache             # cache entries, staleness, hit/miss summary
comb c lifecycle         # keys in the decay/drain sequence
comb c watches           # active filesystem watch registrations
comb c timers            # poll timers and last-run times
comb c demand            # demand-tracked keys and last-query times
comb c procs             # 1-minute process snapshot (-d 2m for longer), categorize against providers
```

Each check prints `[PASS]`, `[WARN]`, or `[FAIL]` with a short explanation.

`comb c config` validates the composed configuration locally (a `FAIL` for each error, a `WARN` for each warning: a client template reading a name no provider has) as well as relaying the daemon's view — locally with the variables a daemon started on it would have, the shell's with the env file's over them — and reports the managed `defaults/` directory without writing any defaults (a `.comb-probe.<pid>` write probe is created and removed to test writability): `[PASS] local    defaults <dir> (<n> files, comb <version>)` when every shipped file is current, `[WARN]` when shipped files are missing or stale (the daemon writes them on its next start) or when the directory holds files `comb` did not write, and `[FAIL]` when the directory is not writable.

`comb c config --origins [KEY]` shows how the composed configuration came together instead, read from the files with no daemon (so it works when the daemon cannot start). Every leaf of the composition — `[daemon]` settings, Source and provider lines, `_` prototypes, `enabled`, `cache._.decay`, `[client.providers.*] template.*` — is a row: its key in key form (`daemon.providers.git.sources._.return`, a name holding a `.` as `template['a.b']`), the file that set it as `status` labels one, `config(<file>)`, and its value. Beneath it, every earlier file that set the same leaf and was overridden is marked `shadowed`, the one it overrode first: `conf.d/` files from last to first, then `config.toml`, then `defaults/`. A setting no file sets but `comb` gives a value of its own shows `built-in` as its source: `daemon.log_level`, `daemon.timeout`, `daemon.budget`, `daemon.env_file` and `daemon.plugin.path` (both beside `config.toml`), and `daemon.watchdog_threshold` when an interval is set (three of it); one `comb` gives no value (`socket_path`, the watchdog, `idle_shutdown`) has no row. `KEY` limits the rows to that key and what is below it (`--origins daemon.providers.git`, `--origins 'client.providers.aws.template.profile'`).

```
$ comb c config --origins client.providers.aws.template.profile
  KEY                                    SOURCE                                             VALUE
  client.providers.aws.template.profile  config(~/.config/beachcomber/conf.d/10-mine.toml)  {{ env.MY_PROFILE or 'mine' }}
  ├─ shadowed                            config(~/.config/beachcomber/config.toml)          {{ 'from-config' }}
  └─ shadowed                            config(~/.config/beachcomber/defaults/aws.toml)    {{ env.AWS_PROFILE or … }}
```

`-f json` prints one object per leaf, `{"key", "value", "file", "shadowed": [{"file", "value"}…]}`, with `file` the absolute path or `null` for a built-in value; the global `--color` and `--ascii` apply as for `comb status --tree`, which this view draws like. A file that cannot be read or parsed, or breaks a rule, is reported beneath the rows as `comb c config` reports it (on stderr beside `-f json`) and exits `2`; the files that did load are still shown.

`--connect-timeout <DURATION>` bounds how long the check keeps trying to reach the daemon before reporting it unreachable (default `1s`; `0` for a single attempt, or any [duration](#durations), e.g. `250ms`, `2s`). Retries start 5ms apart and double, so a daemon that is mid-restart is reached as soon as it binds. The option is accepted before or after the subcommand: `comb c --connect-timeout 0 daemon`.

**Exit codes:** `0` if healthy, `1` if something is worth a look (a `WARN`), `2` on a `FAIL` or an error.

## `comb d` (daemon) `[--no-reap] [--ready-fd <FD>]`

Run the daemon in the foreground. You almost never need this — the daemon is socket-activated automatically. Use it for debugging or for running under a process supervisor.

Socket path resolution order, the same for every command and every SDK: the global `--socket <PATH>` flag (any command takes it, before or after the subcommand) → `daemon.socket_path` in config → `BEACHCOMBER_SOCKET` env var → `/tmp/beachcomber-<uid>/sock`. The SDKs read no config file, so for them it is the variable, then the default (`bc_socket_path` returns it).

```sh
# Run with debug logging
RUST_LOG=beachcomber=debug comb d

# Override socket path
comb d --socket /tmp/beachcomber-debug.sock
```

The daemon exits cleanly on SIGINT (Ctrl+C) or SIGTERM. Prefer `comb kill` from another shell rather than signalling manually.

**One daemon per user.** The daemon whose socket matches its own resolution above is the *canonical* daemon. It removes orphaned `comb daemon` processes (reparented to PID 1, on sockets nothing resolves) at startup and hourly. A foreground debug run under your shell is never touched; a daemon you run under a supervisor on a *non-default* socket is parented to PID 1, so mark it exempt with `--no-reap`. Full rules in the repo's `docs/canon/singleton.md`.

**Readiness.** `--ready-fd <FD>` names a descriptor the daemon inherited (3 or above): it writes a newline there once its socket is served, or once it finds a daemon of its own build already serving it, and closes it. A daemon that exits first closes it unwritten, so whoever holds the other end learns of the exit at once. comb and every SDK start a daemon this way, and a supervisor can too.

## `comb k` (kill) `[--timeout <duration>]`

Stop the running daemon. Sends `SIGTERM` and waits for the daemon to exit; the next `comb` query will socket-activate a fresh daemon. No-op if the daemon is not running.

```sh
comb kill                 # default 5s timeout
comb k                    # short form
comb kill --timeout 30s   # wait longer on slow shutdowns
comb kill --socket /tmp/beachcomber-debug.sock   # target a custom socket (the global flag)
```

The command asks the daemon for its PID (`introspect{subject:"daemon"}`); there is no PID file to go stale.

**Exit codes:** `0` on success (including no-op), `2` on error (the daemon did not exit within `--timeout`, its pid could not be determined, the signal failed).
